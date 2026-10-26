#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Filesystem,
    Pack,
    Archive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Sync,
    Async,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PriorityClass {
    /// Lowest priority: unzip, ingest, preloading, warming.
    Background,
    /// Runtime on-demand async loading.
    ForegroundAsync,
    /// Sync APIs and startup-critical loads blocking current execution.
    ForegroundBlocking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolKind {
    Fs,
    Pack,
    Image,
    /// Zip extraction. CPU-bound on inflate with a working set of tens of
    /// kilobytes, so it parallelises nearly linearly and is safe to run
    /// several at a time.
    Archive,
    /// Package ingest. Shares the archive machinery but not its cost profile:
    /// transcoding one image holds the encoded bytes, the decoded RGBA, the
    /// ETC2 blocks and the KTX2 container at once — tens of MB for a 2048²
    /// asset, over a hundred for a 4096² one. It gets its own class so that
    /// staying serial, which it must, does not also hold extraction back.
    Ingest,
    /// Platform TLS verification can block in native/JVM certificate APIs.
    /// Bound this class independently so it cannot occupy every IO worker.
    Tls,
    /// Bounded codec/filter steps for active audio streams.
    Audio,
}

impl PoolKind {
    pub(crate) fn is_cpu_bound(self) -> bool {
        matches!(
            self,
            Self::Pack | Self::Image | Self::Archive | Self::Ingest | Self::Audio
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IoRequest {
    ReadFile {
        backend: BackendKind,
        request: RequestKind,
        priority: PriorityClass,
        /// Upper bound on the bytes this read will produce. This is the only
        /// input to the cheap/expensive decision.
        ///
        /// There used to be a `ReadSpec` beside it distinguishing whole-file
        /// from ranged reads, with the range's length narrowing the inline
        /// threshold. It could not change any outcome: the estimate is already
        /// bounded by the requested length, so narrowing the threshold to that
        /// same length was implied. It also carried a `position` that nothing
        /// read and every caller filled with `0`.
        estimated_bytes: usize,
    },
    GetFileInfo {
        backend: BackendKind,
        request: RequestKind,
        priority: PriorityClass,
    },
    DecodeImage {
        backend: BackendKind,
        request: RequestKind,
        priority: PriorityClass,
        encoded_bytes: usize,
        cache_hit: bool,
    },
    Unzip {
        backend: BackendKind,
        priority: PriorityClass,
        compressed_bytes: usize,
    },
    PackageIngest {
        priority: PriorityClass,
        compressed_bytes: usize,
    },
    /// Startup-time exact package verification. Unlike generic synchronous
    /// filesystem operations, this always runs on the bounded FS pool because
    /// tree enumeration and hashing are never cheap enough for the host thread.
    VerifyPackage { priority: PriorityClass },
    StorageGet {
        request: RequestKind,
        priority: PriorityClass,
        estimated_bytes: usize,
    },
    StorageMutate {
        request: RequestKind,
        priority: PriorityClass,
    },
    StorageInfo {
        request: RequestKind,
        priority: PriorityClass,
    },
    /// Generic filesystem operation that isn't a plain ReadFile:
    /// writes, appends, fd writes, copy, mkdir, unlink, rename, rmdir,
    /// truncate, and metadata (stat/access/readdir/list). Routed through
    /// the scheduler so these blocking ops respect domain-close, priority,
    /// backpressure and metrics instead of escaping onto tokio's unbounded
    /// blocking pool.
    FsOp {
        request: RequestKind,
        priority: PriorityClass,
    },
}

impl IoRequest {
    pub fn priority(&self) -> PriorityClass {
        match self {
            IoRequest::ReadFile { priority, .. }
            | IoRequest::GetFileInfo { priority, .. }
            | IoRequest::DecodeImage { priority, .. }
            | IoRequest::Unzip { priority, .. }
            | IoRequest::PackageIngest { priority, .. }
            | IoRequest::VerifyPackage { priority }
            | IoRequest::StorageGet { priority, .. }
            | IoRequest::StorageMutate { priority, .. }
            | IoRequest::StorageInfo { priority, .. }
            | IoRequest::FsOp { priority, .. } => *priority,
        }
    }
}

impl From<RequestKind> for PriorityClass {
    fn from(kind: RequestKind) -> Self {
        match kind {
            RequestKind::Sync => PriorityClass::ForegroundBlocking,
            RequestKind::Async => PriorityClass::ForegroundAsync,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_request_priority_extracts_correct_class() {
        let read = IoRequest::ReadFile {
            backend: BackendKind::Pack,
            request: RequestKind::Sync,
            priority: PriorityClass::ForegroundBlocking,
            estimated_bytes: 0,
        };
        assert_eq!(read.priority(), PriorityClass::ForegroundBlocking);

        let unzip = IoRequest::Unzip {
            backend: BackendKind::Archive,
            priority: PriorityClass::Background,
            compressed_bytes: 0,
        };
        assert_eq!(unzip.priority(), PriorityClass::Background);

        let storage = IoRequest::StorageMutate {
            request: RequestKind::Async,
            priority: PriorityClass::ForegroundAsync,
        };
        assert_eq!(storage.priority(), PriorityClass::ForegroundAsync);
    }

    #[test]
    fn priority_class_ordering() {
        assert!(PriorityClass::ForegroundBlocking > PriorityClass::ForegroundAsync);
        assert!(PriorityClass::ForegroundAsync > PriorityClass::Background);
        assert!(PriorityClass::ForegroundBlocking > PriorityClass::Background);
    }
}
