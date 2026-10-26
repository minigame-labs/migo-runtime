//! Thread-local registration ownership. Keep native setup/rollback outside
//! device callbacks and retain the registration for the worker's whole loop.
use std::{marker::PhantomData, rc::Rc};

pub(super) struct Registration<H> {
    handle: Option<H>,
    release: fn(H),
    // MMCSS must be reverted on the same thread that registered it. This
    // remains enforced even for a future handle representation that is Send.
    _thread: PhantomData<Rc<()>>,
}

impl<H> Registration<H> {
    pub(super) fn acquire<E>(
        acquire: impl FnOnce() -> Result<H, E>,
        configure: impl FnOnce(&H) -> Result<(), E>,
        release: fn(H),
    ) -> Result<Self, E> {
        let guard = Self {
            handle: Some(acquire()?),
            release,
            _thread: PhantomData,
        };
        // A rejected configuration or an unwind releases an acquired handle.
        // On success the caller owns the guard until its worker loop exits.
        configure(guard.handle.as_ref().expect("new registration"))?;
        Ok(guard)
    }
}

impl<H> Drop for Registration<H> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            (self.release)(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    type History = Rc<RefCell<Vec<&'static str>>>;

    fn release(history: History) {
        history.borrow_mut().push("release");
    }

    #[test]
    fn successful_registration_lives_until_its_worker_scope_exits() {
        let history = History::default();
        let guard = Registration::acquire(
            || {
                history.borrow_mut().push("acquire");
                Ok::<_, ()>(history.clone())
            },
            |history| {
                history.borrow_mut().push("configure");
                Ok(())
            },
            release,
        )
        .unwrap();
        assert_eq!(&*history.borrow(), &["acquire", "configure"]);
        history.borrow_mut().push("callback");
        drop(guard);
        assert_eq!(
            &*history.borrow(),
            &["acquire", "configure", "callback", "release"]
        );
    }

    #[test]
    fn denied_acquisition_does_not_configure_or_release_a_handle() {
        let result = Registration::acquire(
            || Err::<(), _>("denied"),
            |_| panic!("cannot configure an unacquired handle"),
            |_| panic!("cannot release an unacquired handle"),
        );
        assert_eq!(result.err(), Some("denied"));
    }

    #[test]
    fn denied_configuration_reverts_once_before_returning_the_error() {
        let history = History::default();
        let result = Registration::acquire(
            || Ok(history.clone()),
            |history| {
                history.borrow_mut().push("configure");
                Err("denied")
            },
            release,
        );
        assert_eq!(result.err(), Some("denied"));
        assert_eq!(&*history.borrow(), &["configure", "release"]);
    }

    #[test]
    fn configuration_unwind_also_reverts_once() {
        let history = History::default();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Registration::acquire::<()>(
                || Ok(history.clone()),
                |_| panic!("configuration unwound"),
                release,
            )
        }));
        assert!(result.is_err());
        assert_eq!(&*history.borrow(), &["release"]);
    }
}
