//! Streaming media extents share the control reader's representation identity,
//! but never its small allocation/total-byte limits or whole-exchange timeout.
use super::{Error, Reader, Segment};
use bytes::Bytes;
use std::{borrow::BorrowMut, time::Duration};
use tokio::time::Instant;

#[derive(Clone, Copy, Debug)]
pub struct StreamLimits {
    /// Bounds amplification by peers returning arbitrarily short valid 206s.
    pub max_requests: u32,
    pub header_timeout: Duration,
    /// Applied to each polled body read, excluding downstream backpressure.
    pub idle_timeout: Duration,
}
impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            max_requests: 4096,
            header_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(30),
        }
    }
}

/// A finite byte extent, without collecting it. This owns at most one live HTTP
/// body; yielded Bytes retain the client's storage without another copy.
///
/// A short 206 is continued only after its strict body EOF. None proves the
/// requested extent ended, not that an unknown-length representation ended.
/// Earlier yielded chunks cannot be retracted if a later body read fails.
/// Dropping a pending chunk future poisons the parent reader; dropping a stream
/// between completed reads simply closes its current response.
pub struct MediaStream<R> {
    reader: R,
    limits: StreamLimits,
    position: u64,
    end: u64,
    segment: Option<Segment>,
    requests: u32,
    bytes: u64,
}
/// Borrowed streaming view; the parent reader is available again on drop.
pub type Stream<'a> = MediaStream<&'a mut Reader>;
/// Owned streaming view; reclaim its exact reader to resolve control requests.
pub type OwnedStream = MediaStream<Reader>;

impl Reader {
    /// Stream an explicitly known extent. Its size and positions remain u64;
    /// no buffer allocation or media-duration cap follows from this length.
    /// Control/index counters and budgets are left untouched.
    pub fn stream_at(
        &mut self,
        offset: u64,
        len: u64,
        limits: StreamLimits,
    ) -> Result<Stream<'_>, Error> {
        MediaStream::new(self, offset, len, limits)
    }

    /// Move this reader into a media response without cloning its identity,
    /// resetting counters, allocating an owner, or using self-references.
    pub fn into_stream_at(
        self,
        offset: u64,
        len: u64,
        limits: StreamLimits,
    ) -> Result<OwnedStream, Error> {
        MediaStream::new(self, offset, len, limits)
    }
}
impl OwnedStream {
    pub fn reader(&self) -> &Reader {
        &self.reader
    }
    /// Closes the current body. Cancellation/failure remains latched, as do
    /// learned object length and all control-read counters and byte budgets.
    pub fn into_reader(self) -> Reader {
        self.reader
    }
}
impl<R: BorrowMut<Reader>> MediaStream<R> {
    fn new(mut owner: R, offset: u64, len: u64, limits: StreamLimits) -> Result<Self, Error> {
        let reader = owner.borrow_mut();
        if reader.failed {
            return Err(Error::PreviouslyFailed);
        }
        reader.failed = true;
        let end = offset
            .checked_add(len)
            .filter(|end| *end > offset)
            .ok_or(Error::Limit)?;
        if reader.identity.length.is_some_and(|n| end > n)
            || limits.header_timeout.is_zero()
            || limits.idle_timeout.is_zero()
        {
            return Err(Error::Limit);
        }
        reader.failed = false;
        Ok(Self {
            reader: owner,
            limits,
            position: offset,
            end,
            segment: None,
            requests: 0,
            bytes: 0,
        })
    }
    pub fn position(&self) -> u64 {
        self.position
    }
    pub fn requests(&self) -> u32 {
        self.requests
    }
    pub fn bytes_received(&self) -> u64 {
        self.bytes
    }
    pub fn object_length(&self) -> Option<u64> {
        self.reader.borrow().length()
    }

    pub async fn chunk(&mut self) -> Result<Option<Bytes>, Error> {
        if self.reader.borrow_mut().failed {
            return Err(Error::PreviouslyFailed);
        }
        // A cancelled future must not retry an unknown partially consumed body.
        self.reader.borrow_mut().failed = true;
        let result = self.next_inner().await;
        if result.is_ok() {
            self.reader.borrow_mut().failed = false;
        } else {
            self.segment = None;
        }
        result
    }

    async fn next_inner(&mut self) -> Result<Option<Bytes>, Error> {
        loop {
            if let Some(segment) = &mut self.segment {
                let deadline = Instant::now()
                    .checked_add(self.limits.idle_timeout)
                    .ok_or(Error::Limit)?;
                if let Some(chunk) = segment.chunk(deadline).await? {
                    self.position += chunk.len() as u64;
                    self.bytes += chunk.len() as u64;
                    return Ok(Some(chunk));
                }
                self.segment = None;
            }
            if self.position == self.end {
                return Ok(None);
            }
            self.requests = self
                .requests
                .checked_add(1)
                .filter(|n| *n <= self.limits.max_requests)
                .ok_or(Error::Limit)?;
            let deadline = Instant::now()
                .checked_add(self.limits.header_timeout)
                .ok_or(Error::Limit)?;
            self.segment = Some(
                self.reader
                    .borrow_mut()
                    .open_range(self.position, self.end, deadline, false)
                    .await?,
            );
        }
    }
}
