//! Streaming media extents share the control reader's representation identity,
//! but never its small allocation/total-byte limits or whole-exchange timeout.
use super::{Error, Reader, Segment};
use bytes::Bytes;
use std::time::Duration;
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
pub struct Stream<'a> {
    reader: &'a mut Reader,
    limits: StreamLimits,
    position: u64,
    end: u64,
    segment: Option<Segment>,
    requests: u32,
    bytes: u64,
}
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
        if self.failed {
            return Err(Error::PreviouslyFailed);
        }
        self.failed = true;
        let end = offset
            .checked_add(len)
            .filter(|end| *end > offset)
            .ok_or(Error::Limit)?;
        if self.identity.length.is_some_and(|n| end > n)
            || limits.header_timeout.is_zero()
            || limits.idle_timeout.is_zero()
        {
            return Err(Error::Limit);
        }
        self.failed = false;
        Ok(Stream {
            reader: self,
            limits,
            position: offset,
            end,
            segment: None,
            requests: 0,
            bytes: 0,
        })
    }
}
impl Stream<'_> {
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
        self.reader.length()
    }

    pub async fn chunk(&mut self) -> Result<Option<Bytes>, Error> {
        if self.reader.failed {
            return Err(Error::PreviouslyFailed);
        }
        // A cancelled future must not retry an unknown partially consumed body.
        self.reader.failed = true;
        let result = self.next_inner().await;
        if result.is_ok() {
            self.reader.failed = false;
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
                    .open_range(self.position, self.end, deadline, false)
                    .await?,
            );
        }
    }
}
