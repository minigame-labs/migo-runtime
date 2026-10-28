//! Reusable ranges over the package's validated index and pinned file.
//! Partial ranges do not prove the entry CRC; read_entry verifies a whole entry.
use super::*;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub struct PackageReadLimits {
    pub max_encoded_chunk: usize,
    pub max_decoded_chunk: usize,
}
impl Default for PackageReadLimits {
    fn default() -> Self {
        Self {
            max_encoded_chunk: zstd::zstd_safe::compress_bound(MAX_CHUNK_RAW_SIZE as usize),
            max_decoded_chunk: MAX_CHUNK_RAW_SIZE as usize,
        }
    }
}

pub struct PackageEntryReader {
    inner: Arc<ReaderInner>,
    entry: EntryMeta,
    compressed: Vec<u8>,
    decoded: Vec<u8>,
    decoder: zstd::bulk::Decompressor<'static>,
    cached: Option<u32>,
    failed: bool,
    #[cfg(test)]
    decodes: usize,
    #[cfg(test)]
    after_read: Option<Box<dyn FnOnce() + Send>>,
}
impl PackageEntryReader {
    pub(super) fn new(
        inner: Arc<ReaderInner>,
        path: &str,
        limits: PackageReadLimits,
    ) -> Result<Self, PackageError> {
        inner.stamp.verify(&inner.file)?;
        let entry = inner
            .entries
            .get(path)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_owned()))?;
        // Validate the complete entry's scratch needs before allocating or reading.
        let hard = crate::protocol::io_cmd::MAX_READ_LENGTH as usize;
        if limits.max_encoded_chunk == 0
            || limits.max_encoded_chunk > hard
            || limits.max_decoded_chunk == 0
            || limits.max_decoded_chunk > MAX_CHUNK_RAW_SIZE as usize
        {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "invalid package read limits").into(),
            );
        }
        let chunks = &inner.chunks
            [entry.first_chunk as usize..(entry.first_chunk + entry.chunk_count) as usize];
        let encoded = chunks
            .iter()
            .map(|c| c.compressed_size as usize)
            .max()
            .unwrap_or(0);
        let decoded = chunks
            .iter()
            .map(|c| c.raw_size as usize)
            .max()
            .unwrap_or(0);
        if encoded > limits.max_encoded_chunk || decoded > limits.max_decoded_chunk {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "package chunk exceeds reader scratch budget",
            )
            .into());
        }
        let entry = entry.clone();
        Ok(Self {
            inner,
            entry,
            compressed: vec![0; encoded],
            decoded: vec![0; decoded],
            decoder: zstd::bulk::Decompressor::new()?,
            cached: None,
            failed: false,
            #[cfg(test)]
            decodes: 0,
            #[cfg(test)]
            after_read: None,
        })
    }
    pub fn len(&self) -> u64 {
        self.entry.raw_size
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Persistent Rust scratch only, excluding the shared index and native zstd context.
    pub fn scratch_bytes(&self) -> usize {
        self.compressed.capacity() + self.decoded.capacity()
    }
    pub fn verify(&self) -> io::Result<()> {
        self.inner.stamp.verify(&self.inner.file)
    }

    /// Fill exactly this entry range. On error discard the destination; this reader
    /// stays failed, including after a bad extent. No new payload allocation.
    pub fn read_exact_at(&mut self, offset: u64, output: &mut [u8]) -> Result<(), PackageError> {
        if self.failed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "package reader previously failed",
            )
            .into());
        }
        self.failed = true;
        let end = offset
            .checked_add(output.len() as u64)
            .filter(|end| *end <= self.entry.raw_size)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "package extent outside entry")
            })?;
        self.verify()?;
        let mut at = offset;
        let mut filled = 0;
        while at < end {
            let stride = self.inner.chunks[self.entry.first_chunk as usize].raw_size as u64;
            let i = (at / stride) as u32;
            let chunk = &self.inner.chunks[(self.entry.first_chunk + i) as usize];
            if self.cached != Some(i) {
                self.cached = None;
                let compressed = &mut self.compressed[..chunk.compressed_size as usize];
                read_exact_at(&self.inner.file, compressed, chunk.data_offset)?;
                // One frame per writer chunk: reject concatenated frames and suffixes.
                let frame_size =
                    zstd::zstd_safe::find_frame_compressed_size(compressed).map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "invalid package zstd frame")
                    })?;
                if frame_size != compressed.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "package chunk has trailing frames or bytes",
                    )
                    .into());
                }
                let size = self.decoder.decompress_to_buffer(
                    compressed,
                    &mut self.decoded[..chunk.raw_size as usize],
                )?;
                if size != chunk.raw_size as usize {
                    return Err(PackageError::DecompressSizeMismatch {
                        path: self.entry.path.clone(),
                        expected: chunk.raw_size as u64,
                        actual: size,
                    });
                }
                #[cfg(test)]
                {
                    self.decodes += 1;
                }
                self.cached = Some(i);
            }
            let within = (at % stride) as usize;
            let len = ((end - at) as usize).min(chunk.raw_size as usize - within);
            output[filled..filled + len].copy_from_slice(&self.decoded[within..within + len]);
            filled += len;
            at += len as u64;
        }
        #[cfg(test)]
        if let Some(hook) = self.after_read.take() {
            hook();
        }
        self.verify()?;
        self.failed = false;
        Ok(())
    }
}
