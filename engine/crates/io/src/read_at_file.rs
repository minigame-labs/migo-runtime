//! Bounded positioned reads from an already-open regular file.
//!
//! VFS callers pass its anchored handle, never re-open a pathname per chunk.
//! Unix reads leave the cursor untouched; Windows reads use an explicit offset
//! on this owned handle. Metadata guards detect observed
//! mutation; they do not turn a writable inode into an immutable snapshot.
//! Streaming callers must use immutable code assets or a private spool file.
use parking_lot::Mutex;
use shared::vfs::file_io::{FileStamp as Stamp, read_exact_at};
use std::{fs::File, io, sync::Arc};

pub const FILE_CHUNK_BYTES: usize = 64 * 1024;
struct BufferState {
    slots: [Option<Vec<u8>>; 2],
    allocated: usize,
}
struct Buffers {
    state: Mutex<BufferState>,
    _lease: Option<crate::pools::ByteTicket>,
}
pub(crate) struct ChunkPool(Arc<Buffers>);
impl ChunkPool {
    pub(crate) fn new(lease: Option<crate::pools::ByteTicket>) -> Self {
        Self::from_parts(vec![0; FILE_CHUNK_BYTES], lease)
    }
    fn from_parts(buffer: Vec<u8>, lease: Option<crate::pools::ByteTicket>) -> Self {
        Self(Arc::new(Buffers {
            state: Mutex::new(BufferState {
                slots: [Some(buffer), None],
                allocated: 1,
            }),
            _lease: lease,
        }))
    }
    fn from_buffer(buffer: Vec<u8>, lease: crate::pools::ByteTicket) -> io::Result<Self> {
        if buffer.len() != FILE_CHUNK_BYTES || buffer.capacity() != FILE_CHUNK_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid media scratch buffer",
            ));
        }
        Ok(Self::from_parts(buffer, Some(lease)))
    }
    pub(crate) fn fill(
        &self,
        len: usize,
        fill: impl FnOnce(&mut [u8]) -> io::Result<()>,
    ) -> io::Result<FileChunk> {
        if len > FILE_CHUNK_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "media chunk exceeds bound",
            ));
        }
        let data = {
            let mut state = self.0.state.lock();
            if let Some(slot) = state.slots.iter_mut().find(|slot| slot.is_some()) {
                slot.take().unwrap()
            } else if state.allocated < 2 {
                state.allocated += 1;
                drop(state);
                vec![0; FILE_CHUNK_BYTES]
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "both file chunks are still in flight",
                ));
            }
        };
        let mut chunk = FileChunk {
            data,
            filled: len,
            pool: self.0.clone(),
        };
        fill(&mut chunk.data[..len])?;
        Ok(chunk)
    }
}
pub(crate) fn control_limit(limit: usize) -> io::Result<()> {
    if limit == 0 || limit as u64 > shared::protocol::io_cmd::MAX_READ_LENGTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid file control limit",
        ));
    }
    Ok(())
}
pub(crate) fn check_extent(object_len: u64, offset: u64, len: usize, cap: usize) -> io::Result<()> {
    if len > cap
        || offset
            .checked_add(len as u64)
            .is_none_or(|end| end > object_len)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file read extent exceeds limit",
        ));
    }
    Ok(())
}
pub struct FileChunk {
    data: Vec<u8>,
    filled: usize,
    pool: Arc<Buffers>,
}
impl AsRef<[u8]> for FileChunk {
    fn as_ref(&self) -> &[u8] {
        &self.data[..self.filled]
    }
}
impl Drop for FileChunk {
    fn drop(&mut self) {
        let mut state = self.pool.state.lock();
        if let Some(slot) = state.slots.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(std::mem::take(&mut self.data));
        }
    }
}
pub struct ReadAtFile {
    file: File,
    stamp: Stamp,
    max_control_bytes: usize,
    pool: ChunkPool,
    #[cfg(test)]
    reads: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    after_read: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
impl ReadAtFile {
    /// Construct on the bounded FS executor: metadata is a disk operation.
    pub fn new(file: File, max_control_bytes: usize) -> io::Result<Self> {
        Self::with_pool(file, max_control_bytes, || Ok(ChunkPool::new(None)))
    }
    pub(crate) fn from_buffer(
        file: File,
        max_control_bytes: usize,
        buffer: Vec<u8>,
        lease: crate::pools::ByteTicket,
    ) -> io::Result<Self> {
        Self::with_pool(file, max_control_bytes, || {
            ChunkPool::from_buffer(buffer, lease)
        })
    }
    fn with_pool(
        file: File,
        max_control_bytes: usize,
        pool: impl FnOnce() -> io::Result<ChunkPool>,
    ) -> io::Result<Self> {
        control_limit(max_control_bytes)?;
        let stamp = Stamp::of(file.metadata()?)?;
        Ok(Self {
            file,
            stamp,
            max_control_bytes,
            #[cfg(test)]
            reads: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            after_read: Mutex::new(None),
            pool: pool()?,
        })
    }
    pub fn len(&self) -> u64 {
        self.stamp.len
    }
    pub fn is_empty(&self) -> bool {
        self.stamp.len == 0
    }
    pub fn verify(&self) -> io::Result<()> {
        self.stamp.verify(&self.file)
    }
    fn extent(&self, offset: u64, len: usize, cap: usize) -> io::Result<()> {
        check_extent(self.stamp.len, offset, len, cap)
    }
    fn fill(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.verify()?;
        #[cfg(test)]
        if !buffer.is_empty() {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        read_exact_at(&self.file, buffer, offset)?;
        #[cfg(test)]
        if let Some(hook) = self.after_read.lock().take() {
            hook();
        }
        self.verify()
    }
    pub fn read_exact_at(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.extent(offset, len, self.max_control_bytes)?;
        let mut data = vec![0; len];
        self.fill(offset, &mut data)?;
        Ok(data)
    }
    /// Recycle one media buffer after the caller drops its previous chunk.
    /// One extra slot is allocated lazily for a validated reposition while the
    /// old input is still in flight. A third retained chunk is rejected.
    pub fn chunk_at(&self, offset: u64, len: usize) -> io::Result<FileChunk> {
        self.extent(offset, len, FILE_CHUNK_BYTES)?;
        self.pool.fill(len, |buffer| self.fill(offset, buffer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Seek, SeekFrom, Write},
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn fixture() -> (std::path::PathBuf, Vec<u8>) {
        let p = std::env::temp_dir().join(format!(
            "migo-positioned-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let data = (0..FILE_CHUNK_BYTES)
            .map(|n| (n % 251) as u8)
            .collect::<Vec<_>>();
        std::fs::write(&p, &data).unwrap();
        (p, data)
    }
    #[test]
    fn bounded_controls_rewinds_and_two_media_slots_keep_exact_bytes() {
        let (p, data) = fixture();
        let source = ReadAtFile::new(File::open(&p).unwrap(), 128).unwrap();
        assert_eq!(source.read_exact_at(37, 128).unwrap(), data[37..165]);
        let reads = source.reads.load(Ordering::Relaxed);
        assert!(source.read_exact_at(0, 129).is_err());
        assert!(source.read_exact_at(u64::MAX, 8).is_err());
        assert!(source.read_exact_at(source.len(), 1).is_err());
        assert!(source.chunk_at(0, FILE_CHUNK_BYTES + 1).is_err());
        assert_eq!(
            source.reads.load(Ordering::Relaxed),
            reads,
            "invalid extents reached a payload read"
        );
        let a = source.chunk_at(17, 256).unwrap();
        let b = source.chunk_at(0, 512).unwrap();
        assert_eq!(a.as_ref(), &data[17..273]);
        assert_eq!(b.as_ref(), &data[..512]);
        assert_eq!(
            source.chunk_at(0, 8).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        let pointer = a.as_ref().as_ptr();
        drop(a);
        let c = source.chunk_at(1024, 16).unwrap();
        assert_eq!(pointer, c.as_ref().as_ptr());
        assert_eq!(c.as_ref(), &data[1024..1040]);
        drop(b);
        drop(c);
        assert!(
            source
                .chunk_at(source.len(), 0)
                .unwrap()
                .as_ref()
                .is_empty()
        );
        drop(source);
        std::fs::remove_file(p).unwrap();
    }
    #[test]
    fn short_chunks_hide_retained_bytes_without_rezeroing_storage() {
        let (p, data) = fixture();
        let source = ReadAtFile::new(File::open(&p).unwrap(), 128).unwrap();
        let a = source.chunk_at(64, 128).unwrap();
        assert_eq!(a.as_ref(), &data[64..192]);
        drop(a);
        let b = source.chunk_at(1024, 32).unwrap();
        assert_eq!(b.as_ref(), &data[1024..1056]);
        assert_eq!(b.as_ref().len(), 32);
        drop(b);
        let state = source.pool.0.state.lock();
        let storage = state.slots.iter().find_map(Option::as_ref).unwrap();
        assert_eq!(storage.len(), FILE_CHUNK_BYTES);
        assert_eq!(
            storage[100], data[164],
            "media storage was unnecessarily zeroed again"
        );
        drop(state);
        drop(source);
        std::fs::remove_file(p).unwrap();
    }
    #[test]
    fn mutation_before_read_never_issues_a_payload_read() {
        let (p, _) = fixture();
        let source = ReadAtFile::new(File::open(&p).unwrap(), 128).unwrap();
        File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_len(2)
            .unwrap();
        assert!(source.read_exact_at(0, 8).is_err());
        assert_eq!(source.reads.load(Ordering::Relaxed), 0);
        drop(source);
        std::fs::remove_file(p).unwrap();
    }
    #[test]
    fn mutation_after_read_and_after_last_chunk_never_become_successful_eof() {
        let (p, _) = fixture();
        let source = ReadAtFile::new(File::open(&p).unwrap(), 128).unwrap();
        let target = p.clone();
        *source.after_read.lock() = Some(Box::new(move || {
            File::options()
                .write(true)
                .open(target)
                .unwrap()
                .set_len(7)
                .unwrap()
        }));
        assert!(source.read_exact_at(0, 8).is_err());
        assert_eq!(source.reads.load(Ordering::Relaxed), 1);
        assert!(source.chunk_at(source.len(), 0).is_err());
        drop(source);
        std::fs::remove_file(p).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn positioned_reads_preserve_cursor_and_pinned_inode_after_path_replacement() {
        let (p, data) = fixture();
        let mut cursor = File::open(&p).unwrap();
        cursor.seek(SeekFrom::Start(29)).unwrap();
        let source = ReadAtFile::new(cursor.try_clone().unwrap(), 128).unwrap();
        assert_eq!(source.read_exact_at(7, 10).unwrap(), data[7..17]);
        assert_eq!(cursor.stream_position().unwrap(), 29);
        let replacement = p.with_extension("replacement");
        std::fs::write(&replacement, vec![9; FILE_CHUNK_BYTES]).unwrap();
        std::fs::rename(&replacement, &p).unwrap();
        assert_eq!(source.read_exact_at(7, 10).unwrap(), data[7..17]);
        drop(source);
        std::fs::remove_file(p).unwrap();
    }
    #[test]
    fn large_sparse_offsets_are_u64_and_do_not_read_the_prefix() {
        let (p, _) = fixture();
        let offset = (1u64 << 33) + 31;
        let mut f = File::options().write(true).open(&p).unwrap();
        f.seek(SeekFrom::Start(offset)).unwrap();
        f.write_all(b"positioned").unwrap();
        drop(f);
        let source = ReadAtFile::new(File::open(&p).unwrap(), 128).unwrap();
        assert_eq!(source.read_exact_at(offset, 10).unwrap(), b"positioned");
        assert_eq!(source.reads.load(Ordering::Relaxed), 1);
        drop(source);
        std::fs::remove_file(p).unwrap();
    }
    #[test]
    fn recycled_media_reads_have_no_steady_rust_heap_allocation() {
        let (p, data) = fixture();
        let source = ReadAtFile::new(File::open(&p).unwrap(), 128).unwrap();
        let a = source.chunk_at(0, 128).unwrap();
        let b = source.chunk_at(128, 128).unwrap();
        drop(a);
        drop(b);
        migo_alloc_probe::assert_no_steady_state_allocation(
            migo_alloc_probe::Burst {
                path: "positioned file recycling",
                warmup: 8,
                measured: 64,
            },
            |n| {
                let offset = (n * 137) % 32000;
                let chunk = source.chunk_at(offset as u64, 128).unwrap();
                assert_eq!(chunk.as_ref(), &data[offset..offset + 128]);
            },
        );
        drop(source);
        std::fs::remove_file(p).unwrap();
    }
}
