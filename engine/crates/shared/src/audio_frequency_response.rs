//! Keep native filter storage and its admission alive through serialization.
//! Consumers borrow the two arrays; ownership cannot be split from its credit.
use serde::{Serialize, Serializer};

pub trait FrequencyResponseStorage: Send {
    fn values(&self) -> (&[f32], &[f32]);
}
impl FrequencyResponseStorage for (Vec<f32>, Vec<f32>) {
    fn values(&self) -> (&[f32], &[f32]) {
        (&self.0, &self.1)
    }
}

pub struct FrequencyResponse(Box<dyn FrequencyResponseStorage>);
impl FrequencyResponse {
    pub fn new(storage: impl FrequencyResponseStorage + 'static) -> Self {
        Self(Box::new(storage))
    }
    pub fn values(&self) -> (&[f32], &[f32]) {
        self.0.values()
    }
}
impl std::fmt::Debug for FrequencyResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("FrequencyResponse")
            .field(&self.values())
            .finish()
    }
}
impl Serialize for FrequencyResponse {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.values().serialize(serializer)
    }
}

#[cfg(test)]
pub(crate) fn serialization_owns_storage() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct Storage {
        data: (Vec<f32>, Vec<f32>),
        dropped: Arc<AtomicBool>,
    }
    impl FrequencyResponseStorage for Storage {
        fn values(&self) -> (&[f32], &[f32]) {
            assert!(
                !self.dropped.load(Ordering::Acquire),
                "serialization used dropped response storage"
            );
            (&self.data.0, &self.data.1)
        }
    }
    impl Drop for Storage {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }
    let data = (
        (0..16384).map(|i| i as f32 * 0.25).collect::<Vec<_>>(),
        (0..16384).map(|i| -(i as f32) * 0.125).collect::<Vec<_>>(),
    );
    let want = serde_json::to_vec(&data).unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let response = FrequencyResponse::new(Storage {
        data,
        dropped: dropped.clone(),
    });
    let json = serde_json::to_vec(&response).unwrap();
    assert_eq!(json, want, "response JSON shape changed");
    assert!(
        !dropped.load(Ordering::Acquire),
        "serialization detached response storage"
    );
    struct Refuse;
    impl std::io::Write for Refuse {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("serializer cancelled"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(serde_json::to_writer(Refuse, &response).is_err());
    assert!(
        !dropped.load(Ordering::Acquire),
        "failed serialization detached response storage"
    );
    drop(response);
    assert!(
        dropped.load(Ordering::Acquire),
        "response storage leaked after serialization"
    );
}
