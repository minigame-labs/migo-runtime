use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use parking_lot::Mutex;
use shared::{
    error::EngineError,
    protocol::io_cmd::{FileId, FileStat, OpenFlag},
};

use crate::fs_ops::FileTable;

pub struct IoDomain {
    state: Mutex<DomainState>,
    closed: AtomicBool,
    #[cfg(test)]
    register_temp_file_hook: Mutex<Option<RegisterTempFileHook>>,
}

#[derive(Debug)]
pub enum DomainError {
    Closed,
    Io(EngineError),
}

struct DomainState {
    closed: bool,
    file_table: FileTable,
    temp_files: HashSet<PathBuf>,
}

impl IoDomain {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(DomainState {
                closed: false,
                file_table: FileTable::new(),
                temp_files: HashSet::new(),
            }),
            closed: AtomicBool::new(false),
            #[cfg(test)]
            register_temp_file_hook: Mutex::new(None),
        }
    }

    pub fn with_file_table<T>(
        &self,
        f: impl FnOnce(&mut FileTable) -> T,
    ) -> Result<T, DomainError> {
        self.ensure_open()?;
        let mut state = self.state.lock();
        if state.closed {
            return Err(DomainError::Closed);
        }
        Ok(f(&mut state.file_table))
    }

    pub fn open_file(
        &self,
        path: &Path,
        flag: OpenFlag,
        cleanup_path: Option<PathBuf>,
        synthetic_stat: Option<FileStat>,
    ) -> Result<FileId, DomainError> {
        let cleanup_path_for_table = cleanup_path.clone();
        let rid = self
            .with_file_table(|table| {
                table.open(
                    &path.to_string_lossy(),
                    flag,
                    cleanup_path_for_table,
                    synthetic_stat,
                )
            })?
            .map_err(DomainError::from)?;
        if let Some(path) = cleanup_path {
            self.register_temp_file(path);
        }
        Ok(rid)
    }

    pub fn close_file(&self, id: FileId) -> Result<(), DomainError> {
        let cleanup_path = self
            .with_file_table(|table| table.close_with_cleanup(id))?
            .map_err(DomainError::from)?;
        if let Some(path) = cleanup_path {
            self.unregister_temp_file(&path);
        }
        Ok(())
    }

    pub fn read_file(
        &self,
        id: FileId,
        len: u64,
        position: Option<u64>,
    ) -> Result<Vec<u8>, DomainError> {
        self.with_file_table(|table| table.read(id, len, position))?
            .map_err(DomainError::from)
    }

    /// Read owned bytes bounded by a previously validated destination view.
    pub fn read_file_for_buffer(
        &self,
        id: FileId,
        len: usize,
        position: Option<u64>,
    ) -> Result<crate::fs_ops::OwnedFileRead, DomainError> {
        self.with_file_table(|table| table.read_for_buffer(id, len, position))?
            .map_err(DomainError::from)
    }

    pub fn write_file(
        &self,
        id: FileId,
        data: &[u8],
        position: Option<u64>,
    ) -> Result<usize, DomainError> {
        self.with_file_table(|table| table.write(id, data, position))?
            .map_err(DomainError::from)
    }

    /// Read into an exclusively borrowed destination, without allocation.
    /// The table lock protects the fd; the caller owns destination exclusivity.
    pub fn read_file_into(
        &self,
        id: FileId,
        buf: &mut [u8],
        position: Option<u64>,
    ) -> Result<usize, DomainError> {
        self.with_file_table(|table| table.read_into(id, buf, position))?
            .map_err(DomainError::from)
    }

    pub fn fstat(&self, id: FileId) -> Result<FileStat, DomainError> {
        self.with_file_table(|table| table.fstat(id))?
            .map_err(DomainError::from)
    }

    pub fn ftruncate(&self, id: FileId, len: u64) -> Result<(), DomainError> {
        self.with_file_table(|table| table.ftruncate(id, len))?
            .map_err(DomainError::from)
    }

    pub fn register_temp_file(&self, path: PathBuf) {
        #[cfg(test)]
        self.run_register_temp_file_test_hook();

        if self.is_closed() {
            let _ = std::fs::remove_file(path);
            return;
        }

        let mut state = self.state.lock();
        if state.closed {
            drop(state);
            let _ = std::fs::remove_file(path);
            return;
        }
        state.temp_files.insert(path);
    }

    pub fn unregister_temp_file(&self, path: &Path) {
        self.state.lock().temp_files.remove(path);
    }

    pub fn remove_temp_file(&self, path: &Path) {
        self.unregister_temp_file(path);
        let _ = std::fs::remove_file(path);
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn ensure_open(&self) -> Result<(), DomainError> {
        if self.is_closed() {
            Err(DomainError::Closed)
        } else {
            Ok(())
        }
    }

    pub fn close_all(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }

        let temp_files = {
            let mut state = self.state.lock();
            if state.closed {
                return;
            }

            state.closed = true;
            state.file_table.close_all();

            state.temp_files.drain().collect::<Vec<_>>()
        };

        for path in temp_files {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
type RegisterTempFileHook = std::sync::Arc<dyn Fn() + Send + Sync + 'static>;

#[cfg(test)]
impl IoDomain {
    fn run_register_temp_file_test_hook(&self) {
        let hook = self.register_temp_file_hook.lock().clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    // Per-instance, NOT a process-global slot: each test owns its own
    // IoDomain, so another test's parallel `register_temp_file` can no longer
    // run this test's barrier-based hook and deadlock. The hook drops with the
    // domain, so no teardown guard is needed.
    pub(crate) fn install_register_temp_file_test_hook(&self, hook: RegisterTempFileHook) {
        *self.register_temp_file_hook.lock() = Some(hook);
    }

    pub(crate) fn temp_file_count(&self) -> usize {
        self.state.lock().temp_files.len()
    }
}

impl Default for IoDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl From<EngineError> for DomainError {
    fn from(value: EngineError) -> Self {
        Self::Io(value)
    }
}

impl Drop for IoDomain {
    fn drop(&mut self) {
        self.close_all();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        time::{SystemTime, UNIX_EPOCH},
    };

    use shared::protocol::io_cmd::OpenFlag;

    use crate::domain::IoDomain;

    fn temp_path(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("migo-{label}-{nanos}.tmp"))
    }

    #[test]
    fn close_all_removes_registered_temp_files() {
        let path = temp_path("io-domain-close-all");
        std::fs::write(&path, b"temp").unwrap();

        let domain = IoDomain::new();
        domain.register_temp_file(path.clone());

        domain.close_all();

        assert!(domain.is_closed());
        assert!(!path.exists());
    }

    #[test]
    fn register_temp_file_cannot_insert_after_close_all_begins() {
        let path = temp_path("io-domain-race");
        std::fs::write(&path, b"temp").unwrap();

        let domain = Arc::new(IoDomain::new());
        let entered_hook = Arc::new(Barrier::new(2));
        let allow_register = Arc::new(Barrier::new(2));

        let register_domain = Arc::clone(&domain);
        let register_path = path.clone();
        let register_entered = Arc::clone(&entered_hook);
        let register_continue = Arc::clone(&allow_register);
        domain.install_register_temp_file_test_hook(Arc::new(move || {
            register_entered.wait();
            register_continue.wait();
        }));

        let register_thread = std::thread::spawn(move || {
            register_domain.register_temp_file(register_path);
        });

        entered_hook.wait();
        domain.close_all();
        allow_register.wait();
        register_thread.join().unwrap();

        assert!(domain.is_closed());
        assert!(!path.exists());
        assert_eq!(domain.temp_file_count(), 0);
    }

    #[test]
    fn with_file_table_rejects_access_after_close_all() {
        let domain = IoDomain::new();
        domain.close_all();

        let result = domain.with_file_table(|_| ());

        assert!(matches!(result, Err(super::DomainError::Closed)));
    }

    #[test]
    fn closed_domain_rejects_new_open_file_work() {
        let path = temp_path("io-domain-reject-open");
        std::fs::write(&path, b"temp").unwrap();

        let domain = IoDomain::new();
        domain.close_all();

        let result = domain.open_file(&path, OpenFlag::Read, None, None);

        assert!(matches!(result, Err(super::DomainError::Closed)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn close_all_cleans_up_file_flow_resources() {
        let path = temp_path("io-domain-file-flow");
        std::fs::write(&path, b"temp").unwrap();

        let domain = IoDomain::new();
        let rid = domain
            .open_file(&path, OpenFlag::Read, Some(path.clone()), None)
            .unwrap();

        domain.close_all();

        assert!(matches!(domain.fstat(rid), Err(super::DomainError::Closed)));
        assert!(!path.exists());
    }

    #[test]
    fn close_file_unregisters_temp_file_tracking() {
        let path = temp_path("io-domain-close-file");
        std::fs::write(&path, b"temp").unwrap();

        let domain = IoDomain::new();
        let rid = domain
            .open_file(&path, OpenFlag::Read, Some(path.clone()), None)
            .unwrap();
        assert_eq!(domain.temp_file_count(), 1);

        domain.close_file(rid).unwrap();

        assert_eq!(domain.temp_file_count(), 0);
        assert!(!path.exists());
    }

    #[test]
    fn remove_temp_file_unregisters_tracking() {
        let path = temp_path("io-domain-remove-temp");
        std::fs::write(&path, b"temp").unwrap();

        let domain = IoDomain::new();
        domain.register_temp_file(path.clone());
        assert_eq!(domain.temp_file_count(), 1);

        domain.remove_temp_file(&path);

        assert_eq!(domain.temp_file_count(), 0);
        assert!(!path.exists());
    }
}
