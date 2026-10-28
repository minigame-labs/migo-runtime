//! Shared charged payload, used by prefix replay and spool writes alike.
use bytes::Bytes;
use migo_io::pools::{ByteTicket, IoPools};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::Arc;
const MAX_HTTP_CHUNK_BYTES: usize = 1024 * 1024;
fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, e.to_string())
}
pub(crate) struct BodyChunk {
    pub(crate) data: Bytes,
    _credit: Arc<ByteTicket>,
}
impl AsRef<[u8]> for BodyChunk {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}
impl BodyChunk {
    #[cfg(test)]
    pub(crate) fn credit(&self) -> &Arc<ByteTicket> {
        &self._credit
    }
    pub(crate) fn new(data: Bytes, pools: &IoPools) -> EngineResult<Self> {
        if data.len() > MAX_HTTP_CHUNK_BYTES {
            return Err(error("HTTP spool chunk exceeds bound"));
        }
        let credit = pools.reserve_bytes(data.len() as u64).map_err(error)?;
        Ok(Self {
            data,
            _credit: Arc::new(credit),
        })
    }
    pub(crate) fn slice(&self, start: usize, end: usize) -> Self {
        Self {
            data: self.data.slice(start..end),
            _credit: self._credit.clone(),
        }
    }
}
pub(crate) enum Chunk<T> {
    Prefix(BodyChunk),
    Tail(T),
}
impl<T: AsRef<[u8]>> AsRef<[u8]> for Chunk<T> {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Prefix(v) => v.as_ref(),
            Self::Tail(v) => v.as_ref(),
        }
    }
}
