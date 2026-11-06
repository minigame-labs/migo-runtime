//! One immutable URL allocation serves both cache indexes and keeps its credit.
use crate::limits::AudioAggregatePermit;
use crate::node_retirement::{Host, Value};
use shared::error::EngineResult;
use std::{
    alloc::Layout,
    borrow::Borrow,
    fmt,
    hash::{Hash, Hasher},
    sync::{Arc, atomic::AtomicUsize},
};

enum Data {
    Native(String),
    Inline {
        text: String,
        _permit: AudioAggregatePermit,
        #[cfg(test)]
        witness: Witness,
    },
    Cpu(Value<String>),
}
impl Data {
    fn text(&self) -> &String {
        match self {
            Self::Native(text) | Self::Inline { text, .. } => text,
            Self::Cpu(text) => text,
        }
    }
}
pub(super) struct Key(Option<Arc<Data>>);
impl Key {
    // Rust 1.95 ArcInner is repr(C): strong, weak, payload. No Weak escapes.
    fn cell_bytes() -> usize {
        Layout::new::<[AtomicUsize; 2]>()
            .extend(Layout::new::<Data>())
            .unwrap()
            .0
            .pad_to_align()
            .size()
    }
    pub(super) fn new(text: String, host: Option<&Host>) -> EngineResult<Self> {
        let data = match host {
            None => Data::Native(text),
            Some(host) => {
                let bytes = text
                    .capacity()
                    .checked_add(Self::cell_bytes())
                    .ok_or_else(crate::node_storage::size_overflow)?;
                if bytes < 4096 {
                    let permit = host.reserve_inline(bytes)?;
                    Data::Inline {
                        text,
                        _permit: permit,
                        #[cfg(test)]
                        witness: Witness::default(),
                    }
                } else {
                    let reservation = host.reserve_value_with_bytes::<String>(bytes)?;
                    Data::Cpu(reservation.finish(text))
                }
            }
        };
        let key = Self(Some(Arc::new(data)));
        #[cfg(test)]
        let key = {
            let mut key = key;
            OBSERVE.with(|slot| {
                if let Some(hook) = slot.borrow().as_ref() {
                    key.observe(hook.clone());
                }
            });
            key
        };
        Ok(key)
    }
    pub(super) fn as_str(&self) -> &str {
        self.0.as_ref().unwrap().text()
    }
    #[cfg(test)]
    fn capacity(&self) -> usize {
        self.0.as_ref().unwrap().text().capacity()
    }
    #[cfg(test)]
    fn observe(&mut self, hook: Arc<dyn Fn() + Send + Sync>) {
        match Arc::get_mut(self.0.as_mut().unwrap()).unwrap() {
            Data::Inline { witness, .. } => witness.0 = Some(hook),
            Data::Cpu(value) => value.observe(hook),
            Data::Native(_) => {}
        }
    }
}
impl Clone for Key {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl Drop for Key {
    fn drop(&mut self) {
        if let Some(cell) = self.0.take() {
            // Every clone uses into_inner: even concurrent final drops free the
            // Arc cell before dropping the string/inline credit or CPU owner.
            drop(Arc::into_inner(cell));
        }
    }
}
impl Borrow<str> for Key {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}
impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}
impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}
impl Eq for Key {}
impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}
impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}
#[cfg(test)]
#[derive(Default)]
struct Witness(Option<Arc<dyn Fn() + Send + Sync>>);
#[cfg(test)]
impl Drop for Witness {
    fn drop(&mut self) {
        if let Some(hook) = &self.0 {
            hook();
        }
    }
}
#[cfg(test)]
#[path = "cache_key_storage_tests.rs"]
mod tests;

#[cfg(test)]
thread_local! {
    static OBSERVE: std::cell::RefCell<Option<Arc<dyn Fn()+Send+Sync>>> = const {std::cell::RefCell::new(None)};
}
#[cfg(test)]
fn observe_new<R>(hook: Arc<dyn Fn() + Send + Sync>, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<Arc<dyn Fn() + Send + Sync>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            OBSERVE.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(OBSERVE.with(|slot| slot.replace(Some(hook))));
    f()
}
