//! HTTP media as a random-access blocking source, the way a browser's media
//! resource fetcher serves its demuxer.
//!
//! One async fetcher owns the network: it writes every received byte into a
//! sparse spool and records the extent. The decode thread reads any offset; one
//! not yet fetched is published as `want` and the reader blocks until it lands.
//! With a strong validator (`range::Identity`) the fetcher jumps to a wanted
//! offset with a pinned Range request, so a format reader can look at a file's
//! tail (Ogg duration, an M4A index after its media) without waiting for the
//! whole body. Without one it only reads on: a wanted offset is reached when
//! the single response gets there, and the object length stays unreported
//! until then, so no reader asks for the tail early.
use crate::media_source::{cancelled, resolve_seek};
use migo_services::network::gate::GateKind;
use migo_services::network::http::{Bytes, HttpClient, Response, range};
use shared::audio_file::{AudioFileCancel, AudioSparseSpool};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;
use symphonia_core::io::MediaSource;

/// A wanted offset this close ahead of the live response is reached by reading
/// on rather than by a new request.
const READ_ON_WINDOW: u64 = 256 * 1024;
/// Fresh requests after a failed body read, per object.
const MAX_RESTARTS: u32 = 8;
/// Upper bound on one blocked wait between cancellation checks.
const WAIT_SLICE: Duration = Duration::from_millis(250);
/// Body reads that stall this long fail the response (and restart it if ranged).
const BODY_IDLE: Duration = Duration::from_secs(30);

/// Sorted, disjoint, non-adjacent byte extents `[start, end)`.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct Extents(Vec<(u64, u64)>);

impl Extents {
    pub(crate) fn insert(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let (mut start, mut end) = (start, end);
        // Merge every extent that touches [start, end).
        let first = self.0.partition_point(|&(_, e)| e < start);
        let mut last = first;
        while last < self.0.len() && self.0[last].0 <= end {
            start = start.min(self.0[last].0);
            end = end.max(self.0[last].1);
            last += 1;
        }
        self.0.splice(first..last, [(start, end)]);
    }
    /// The end of the extent holding `pos`, if one does.
    pub(crate) fn end_from(&self, pos: u64) -> Option<u64> {
        let i = self.0.partition_point(|&(_, e)| e <= pos);
        self.0.get(i).filter(|&&(s, _)| s <= pos).map(|&(_, e)| e)
    }
    pub(crate) fn contains(&self, pos: u64) -> bool {
        self.end_from(pos).is_some()
    }
    /// The first offset at or after `from` and before `len` that holds no data.
    pub(crate) fn gap_from(&self, from: u64, len: u64) -> Option<u64> {
        let at = self.end_from(from).unwrap_or(from);
        (at < len).then_some(at)
    }
    pub(crate) fn covers(&self, len: u64) -> bool {
        len == 0 || self.end_from(0).is_some_and(|e| e >= len)
    }
}

struct State {
    extents: Extents,
    /// The offset a blocked reader waits for.
    want: Option<u64>,
    /// Known up front, or learned when an unknown-length body ends.
    len: Option<u64>,
    failed: Option<String>,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    landed: Condvar,
    wake_fetcher: tokio::sync::Notify,
    store: Box<dyn AudioSparseSpool>,
    cancel: Arc<dyn AudioFileCancel>,
    ranged: bool,
}

impl Shared {
    /// A panic elsewhere never leaves State half-updated: every critical
    /// section is a few field writes.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    fn stopped(&self) -> bool {
        self.cancel.is_cancelled() || self.lock().closed
    }
    fn fail(&self, message: String) {
        let mut state = self.lock();
        state.failed.get_or_insert(message);
        self.landed.notify_all();
    }
}

/// The decode thread's view. Dropping it stops the fetcher.
pub(crate) struct HttpSource {
    shared: Arc<Shared>,
    pos: u64,
}

impl HttpSource {
    /// Take over an open full response (status 200). `spool` creation is
    /// blocking file IO: call this on the decode thread, with `runtime` the
    /// executor the fetcher runs on.
    pub(crate) fn open(
        runtime: &tokio::runtime::Handle,
        client: HttpClient,
        response: Response,
        spool: &dyn shared::audio_file::AudioHttpSpool,
        cancel: Arc<dyn AudioFileCancel>,
    ) -> EngineResult<Self> {
        if response.status().as_u16() != 200 {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                format!("audio response status {}", response.status()),
            ));
        }
        let identity = range::Identity::capture(&response).ok();
        // A decoded (compressed) body's size is not the object's.
        let len = match &identity {
            Some(identity) => identity.length(),
            None if response.has_decoded_representation() => None,
            None => response.content_length(),
        };
        let ranged = identity.as_ref().is_some_and(|i| i.length().is_some());
        let store = spool.sparse(len)?;
        if len.is_some_and(|len| len > store.len()) {
            return Err(EngineError::new(ErrorCode::InputSaturated));
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                extents: Extents::default(),
                want: None,
                len,
                failed: None,
                closed: false,
            }),
            landed: Condvar::new(),
            wake_fetcher: tokio::sync::Notify::new(),
            store,
            cancel,
            ranged,
        });
        runtime.spawn(fetch(shared.clone(), client, response, identity));
        Ok(Self { shared, pos: 0 })
    }

    /// Stop the fetcher and fail any blocked read now (it would otherwise
    /// notice cancellation within one wait slice).
    pub(crate) fn close(&self) {
        close(&self.shared);
    }
}

fn close(shared: &Shared) {
    shared.lock().closed = true;
    shared.landed.notify_all();
    shared.wake_fetcher.notify_one();
}

impl Drop for HttpSource {
    fn drop(&mut self) {
        close(&self.shared);
    }
}

impl Read for HttpSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let shared = &self.shared;
        let mut state = shared.lock();
        loop {
            if shared.cancel.is_cancelled() || state.closed {
                return Err(cancelled());
            }
            if state.len.is_some_and(|len| self.pos >= len) {
                return Ok(0);
            }
            if let Some(end) = state.extents.end_from(self.pos) {
                let n = (end - self.pos).min(buf.len() as u64) as usize;
                drop(state);
                shared
                    .store
                    .read_at(self.pos, &mut buf[..n])
                    .map_err(|error| io::Error::other(error.to_string()))?;
                self.pos += n as u64;
                return Ok(n);
            }
            if let Some(message) = &state.failed {
                return Err(io::Error::other(message.clone()));
            }
            if state.want != Some(self.pos) {
                state.want = Some(self.pos);
                shared.wake_fetcher.notify_one();
            }
            state = shared
                .landed
                .wait_timeout(state, WAIT_SLICE)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }
}

impl Seek for HttpSource {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let len = self.shared.lock().len;
        self.pos = match (to, len) {
            (SeekFrom::End(_), None) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "audio object length is not known yet",
                ));
            }
            (to, len) => resolve_seek(self.pos, len.unwrap_or(u64::MAX), to)?,
        };
        Ok(self.pos)
    }
}

impl MediaSource for HttpSource {
    fn is_seekable(&self) -> bool {
        true
    }
    /// Ranged objects report their length at once; a read-on object only once it
    /// is complete, so no reader seeks to a tail it would have to wait for.
    fn byte_len(&self) -> Option<u64> {
        let state = self.shared.lock();
        let len = state.len?;
        (self.shared.ranged || state.extents.covers(len)).then_some(len)
    }
}

/// The live body the fetcher reads from.
enum Live {
    Initial(Response),
    Range(Box<range::OwnedStream>),
}

impl Live {
    async fn chunk(&mut self) -> Result<Option<Bytes>, String> {
        let read = async {
            match self {
                Self::Initial(response) => response.chunk().await.map_err(|e| e.to_string()),
                Self::Range(stream) => stream.chunk().await.map_err(|e| e.to_string()),
            }
        };
        tokio::time::timeout(BODY_IDLE, read)
            .await
            .map_err(|_| "audio body read timed out".to_owned())?
    }
}

enum Next {
    /// Keep reading the live body.
    ReadOn,
    /// Open a Range request at this offset.
    Open(u64),
    /// Nothing reachable is missing.
    Done,
    /// Wait for a reader to want something.
    Idle,
}

fn next(state: &State, live_pos: Option<u64>, ranged: bool) -> Next {
    let want = state.want.filter(|&w| !state.extents.contains(w));
    let Some(len) = state.len else {
        // Unknown length: only the initial body can fill it, to its end.
        return if live_pos.is_some() {
            Next::ReadOn
        } else {
            Next::Done
        };
    };
    if state.extents.covers(len) {
        return Next::Done;
    }
    if let Some(pos) = live_pos.filter(|&p| p < len && !state.extents.contains(p)) {
        let reach = want.is_none_or(|w| w >= pos && w < pos.saturating_add(READ_ON_WINDOW));
        if reach || !ranged {
            return Next::ReadOn;
        }
    }
    if !ranged {
        // A read-on object has nothing to open; its body ended or failed.
        return if live_pos.is_some() {
            Next::ReadOn
        } else {
            Next::Done
        };
    }
    match want {
        Some(w) => Next::Open(w),
        // Fill the rest from the first gap after the last read, then the start.
        None => match state
            .extents
            .gap_from(live_pos.unwrap_or(0), len)
            .or_else(|| state.extents.gap_from(0, len))
        {
            Some(gap) => Next::Open(gap),
            None => Next::Idle,
        },
    }
}

async fn fetch(
    shared: Arc<Shared>,
    client: HttpClient,
    initial: Response,
    identity: Option<range::Identity>,
) {
    let mut live = Some((Live::Initial(initial), 0u64));
    let mut restarts = 0;
    loop {
        if shared.stopped() {
            return;
        }
        let step = {
            let state = shared.lock();
            next(&state, live.as_ref().map(|(_, pos)| *pos), shared.ranged)
        };
        match step {
            Next::Done => {
                // Settle readers waiting on bytes no response will bring.
                let mut state = shared.lock();
                if state.len.is_none_or(|len| !state.extents.covers(len)) {
                    state
                        .failed
                        .get_or_insert_with(|| "audio body ended early".to_owned());
                }
                shared.landed.notify_all();
                return;
            }
            Next::Idle => {
                let wake = shared.wake_fetcher.notified();
                tokio::pin!(wake);
                wake.as_mut().enable();
                if shared.stopped() {
                    return;
                }
                wake.await;
                continue;
            }
            Next::Open(at) => {
                let (Some(identity), Some(len)) = (identity.clone(), shared.lock().len) else {
                    shared.fail("audio object cannot be read out of order".to_owned());
                    return;
                };
                let reader = range::Reader::new(
                    client.clone(),
                    GateKind::AudioStream,
                    identity,
                    range::Limits::default(),
                );
                match reader.into_stream_at(at, len - at, range::StreamLimits::default()) {
                    Ok(stream) => live = Some((Live::Range(Box::new(stream)), at)),
                    Err(error) => {
                        shared.fail(error.to_string());
                        return;
                    }
                }
            }
            Next::ReadOn => {}
        }
        let Some((body, pos)) = live.as_mut() else {
            continue;
        };
        let pos_now = *pos;
        // Only closing interrupts a body read: dropping a pending read would
        // poison a range stream, and a new `want` is decided between chunks.
        let chunk = {
            let read = body.chunk();
            tokio::pin!(read);
            loop {
                let wake = shared.wake_fetcher.notified();
                tokio::select! {
                    biased;
                    chunk = &mut read => break chunk,
                    _ = wake => {
                        if shared.stopped() {
                            return;
                        }
                    }
                }
            }
        };
        match chunk {
            Ok(Some(bytes)) => {
                let end = pos_now + bytes.len() as u64;
                if shared.lock().len.is_some_and(|len| end > len) {
                    shared.fail("audio body is longer than its length".to_owned());
                    return;
                }
                if let Err(error) = shared.store.write_at(pos_now, &bytes) {
                    shared.fail(error.to_string());
                    return;
                }
                let mut state = shared.lock();
                state.extents.insert(pos_now, end);
                if state.want.is_some_and(|w| state.extents.contains(w)) {
                    state.want = None;
                }
                if let Some((_, pos)) = live.as_mut() {
                    *pos = end;
                }
                shared.landed.notify_all();
            }
            Ok(None) => {
                let ended_at = pos_now;
                live = None;
                let early = {
                    let mut state = shared.lock();
                    // An unknown-length body's end is the object's end.
                    let len = *state.len.get_or_insert(ended_at);
                    shared.landed.notify_all();
                    ended_at < len && !state.extents.covers(len)
                };
                if early {
                    restarts += 1;
                    if !shared.ranged || restarts > MAX_RESTARTS {
                        shared.fail("audio body ended early".to_owned());
                        return;
                    }
                }
            }
            Err(error) => {
                live = None;
                restarts += 1;
                if !shared.ranged || restarts > MAX_RESTARTS {
                    shared.fail(error);
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "media_source_http_tests.rs"]
mod tests;
