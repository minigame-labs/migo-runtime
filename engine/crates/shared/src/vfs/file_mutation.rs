//! Cooperative per-object mutation epochs for runtime-managed file writers.
//!
//! A read epoch is evidence only when every writer participates before any
//! destructive IO. It does not turn an arbitrary host file into a snapshot.
//! Writers never wait for readers, and no registry/state lock spans native IO.
use super::file_io::FileIdentity;
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    fs::File,
    io,
    sync::{Arc, LazyLock, Weak},
};

static REGISTRY: LazyLock<Mutex<HashMap<FileIdentity, Weak<Tracked>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
#[derive(Default)]
struct State {
    revision: u64,
    writers: usize,
    poisoned: bool,
}
struct Tracked {
    identity: FileIdentity,
    state: Mutex<State>,
}
impl Drop for Tracked {
    fn drop(&mut self) {
        let mut registry = REGISTRY.lock();
        // A new registration can replace a zero-strong-count weak entry while
        // this destructor is waiting for the registry. Never remove that owner.
        if registry
            .get(&self.identity)
            .is_some_and(|entry| std::ptr::eq(entry.as_ptr(), self))
        {
            registry.remove(&self.identity);
        }
    }
}
#[derive(Clone)]
pub struct FileMutations(Arc<Tracked>);
impl FileMutations {
    /// Nonregular handles have no snapshot contract and keep their existing IO.
    pub fn for_file(file: &File) -> io::Result<Option<Self>> {
        let Some(identity) = FileIdentity::of_regular(file)? else {
            return Ok(None);
        };
        let mut registry = REGISTRY.lock();
        let tracked = match registry.get(&identity).and_then(Weak::upgrade) {
            Some(tracked) => tracked,
            None => {
                let tracked = Arc::new(Tracked {
                    identity,
                    state: Mutex::new(State::default()),
                });
                registry.insert(identity, Arc::downgrade(&tracked));
                tracked
            }
        };
        Ok(Some(Self(tracked)))
    }
    fn start_write(&self) -> io::Result<()> {
        let mut state = self.0.state.lock();
        if state.poisoned {
            return Err(changed());
        }
        let Some(revision) = state.revision.checked_add(1) else {
            state.poisoned = true;
            return Err(changed());
        };
        let Some(writers) = state.writers.checked_add(1) else {
            state.poisoned = true;
            return Err(changed());
        };
        state.revision = revision;
        state.writers = writers;
        Ok(())
    }
    /// Retained descriptors borrow their registration: no Arc refcount traffic
    /// or native metadata lookup on each write.
    pub fn begin_write(&self) -> io::Result<WriteGuard<'_>> {
        self.start_write()?;
        Ok(WriteGuard(&self.0))
    }
    /// One-shot writers move their registration into the operation guard.
    pub fn into_write(self) -> io::Result<OwnedWriteGuard> {
        self.start_write()?;
        Ok(OwnedWriteGuard(self.0))
    }
    /// Take the epoch before reading length, metadata or payload. Classification
    /// metadata captured before this call is not a snapshot. Keep the same
    /// native handle alive until the copy and final verification finish.
    pub fn read_epoch(&self) -> io::Result<ReadEpoch> {
        let state = self.0.state.lock();
        if state.poisoned {
            return Err(changed());
        }
        if state.writers != 0 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "file is being modified",
            ));
        }
        Ok(ReadEpoch {
            tracked: self.0.clone(),
            revision: state.revision,
        })
    }
}
fn changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "file mutation epoch changed or exhausted",
    )
}
pub struct WriteGuard<'a>(&'a Tracked);
impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        self.0.state.lock().writers -= 1;
    }
}
pub struct OwnedWriteGuard(Arc<Tracked>);
impl Drop for OwnedWriteGuard {
    fn drop(&mut self) {
        self.0.state.lock().writers -= 1;
    }
}
pub struct ReadEpoch {
    tracked: Arc<Tracked>,
    revision: u64,
}
impl ReadEpoch {
    pub fn verify(&self) -> io::Result<()> {
        let state = self.tracked.state.lock();
        if state.poisoned || state.writers != 0 || state.revision != self.revision {
            Err(changed())
        } else {
            Ok(())
        }
    }
}
#[cfg(test)]
mod tests;
