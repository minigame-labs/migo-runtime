//! Strict client RFC7692 negotiation and bounded, resumable codec tasks.
//! Tungstenite owns framing; existing miniz compression and zlib-rs inflate
//! own DEFLATE. Codec state and pending work remain owned across cancelled waits.
use flate2::{Compress, Compression, FlushCompress, Status};
use std::collections::VecDeque;
use zlib_rs::{Inflate as Decompress, InflateFlush as FlushDecompress, Status as InflateStatus};

pub const CHUNK: usize = 64 * 1024;
const WINDOW: usize = 32 * 1024;
const TAIL: &[u8] = &[0, 0, 255, 255];
// A local-only empty final block verifies an exact byte-aligned message end.
// Resetting with the last window restores takeover after this or a peer BFINAL.
const END: &[u8] = &[1, 0, 0, 255, 255];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Negotiated {
    pub server_no_context_takeover: bool,
    pub client_no_context_takeover: bool,
    pub server_max_window_bits: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    Negotiation,
    EncodedLimit,
    DecodedLimit,
    ExpansionLimit,
    Corrupt,
    Allocation,
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WebSocket deflate {self:?}")
    }
}
impl std::error::Error for Fault {}

/// A bare offer deliberately omits client_max_window_bits: this encoder uses
/// the ordinary 15-bit window, and a server may not add that parameter unasked.
pub const OFFER: &str = "permessage-deflate";

pub fn negotiate<'a>(
    offered: bool,
    headers: impl IntoIterator<Item = &'a str>,
) -> Result<Option<Negotiated>, Fault> {
    let mut headers = headers.into_iter();
    let Some(value) = headers.next() else {
        return Ok(None);
    };
    if !offered || headers.next().is_some() {
        return Err(Fault::Negotiation);
    }
    let mut parts = value.split(';').map(|part| part.trim_matches([' ', '\t']));
    if parts.next() != Some(OFFER) {
        return Err(Fault::Negotiation);
    }
    let mut result = Negotiated {
        server_no_context_takeover: false,
        client_no_context_takeover: false,
        server_max_window_bits: 15,
    };
    let mut seen = 0u8;
    for part in parts {
        let (name, value) = match part.split_once('=') {
            Some((name, value)) => (
                name.trim_matches([' ', '\t']),
                Some(value.trim_matches([' ', '\t'])),
            ),
            None => (part, None),
        };
        let bit = match name {
            "server_no_context_takeover" if value.is_none() => {
                result.server_no_context_takeover = true;
                1
            }
            "client_no_context_takeover" if value.is_none() => {
                result.client_no_context_takeover = true;
                2
            }
            "server_max_window_bits" => {
                let value = parameter_value(value.ok_or(Fault::Negotiation)?)?;
                result.server_max_window_bits = match value.as_str() {
                    "8" => 8,
                    "9" => 9,
                    "10" => 10,
                    "11" => 11,
                    "12" => 12,
                    "13" => 13,
                    "14" => 14,
                    "15" => 15,
                    _ => return Err(Fault::Negotiation),
                };
                4
            }
            _ => return Err(Fault::Negotiation),
        };
        if seen & bit != 0 {
            return Err(Fault::Negotiation);
        }
        seen |= bit;
    }
    Ok(Some(result))
}

fn parameter_value(value: &str) -> Result<String, Fault> {
    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        let mut result = String::new();
        let mut bytes = value.as_bytes()[1..value.len() - 1].iter().copied();
        while let Some(byte) = bytes.next() {
            let byte = if byte == b'\\' {
                bytes.next().ok_or(Fault::Negotiation)?
            } else {
                byte
            };
            if !byte.is_ascii_digit() {
                return Err(Fault::Negotiation);
            }
            result.push(char::from(byte));
        }
        Ok(result)
    } else if value.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(value.to_string())
    } else {
        Err(Fault::Negotiation)
    }
}

pub struct Inflater {
    codec: Decompress,
    no_takeover: bool,
    history: VecDeque<u8>,
    output: Vec<u8>,
    encoded: usize,
    limit: usize,
    steps: usize,
    history_position: usize,
}

struct Step {
    consumed: usize,
    produced: usize,
    full: bool,
    ended: bool,
}

impl Inflater {
    pub fn new(no_takeover: bool, limit: usize) -> Self {
        Self {
            codec: Decompress::new(false, 15),
            no_takeover,
            history: VecDeque::new(),
            output: Vec::new(),
            encoded: 0,
            limit,
            steps: 0,
            history_position: 0,
        }
    }

    /// A caller owns this task across cancelled waits. Native calls are bounded
    /// by CHUNK and each future poll spends at most CHUNK input and CHUNK output.
    pub fn fragment_job<'a>(
        &'a mut self,
        bytes: &'a [u8],
        fin: bool,
    ) -> Result<InflateJob<'a>, Fault> {
        self.encoded = self
            .encoded
            .checked_add(bytes.len())
            .ok_or(Fault::EncodedLimit)?;
        if self.encoded > encoded_limit(self.limit) {
            return Err(Fault::EncodedLimit);
        }
        Ok(InflateJob {
            state: self,
            bytes,
            fin,
            phase: 0,
            cursor: 0,
            ends: 0,
            finished: false,
        })
    }

    /// Synchronous driver for this isolated contract/throughput experiment.
    /// Product integration drives fragment_job on the existing IO executor.
    #[cfg(test)]
    pub fn fragment(&mut self, bytes: &[u8], fin: bool) -> Result<Option<Vec<u8>>, Fault> {
        use std::{
            future::Future,
            pin::Pin,
            task::{Context, Poll, Waker},
        };
        let mut job = self.fragment_job(bytes, fin)?;
        let mut context = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(result) = Pin::new(&mut job).poll(&mut context) {
                return result;
            }
        }
    }

    fn save_history(history: &mut VecDeque<u8>, bytes: &[u8]) {
        if bytes.len() >= WINDOW {
            history.clear();
            history.extend(&bytes[bytes.len() - WINDOW..]);
        } else {
            let remove = (history.len() + bytes.len()).saturating_sub(WINDOW);
            history.drain(..remove);
            history.extend(bytes);
        }
    }

    fn step(
        &mut self,
        bytes: &[u8],
        input_budget: usize,
        output_budget: usize,
        checking_end: bool,
    ) -> Result<Step, Fault> {
        let allowed = self
            .limit
            .saturating_add(1)
            .saturating_sub(self.output.len())
            .min(output_budget);
        if allowed == 0 {
            return Err(Fault::DecodedLimit);
        }
        let needed = self.output.len() + allowed;
        if self.output.capacity() < needed {
            let target = self
                .output
                .capacity()
                .max(1024)
                .saturating_mul(2)
                .max(needed)
                .min(self.limit.saturating_add(1));
            self.output
                .try_reserve_exact(target - self.output.len())
                .map_err(|_| Fault::Allocation)?;
        }
        let supplied = bytes.len().min(input_budget);
        let before_in = self.codec.total_in();
        let before_out = self.codec.total_out();
        let result = self.codec.decompress_uninit(
            &bytes[..supplied],
            &mut self.output.spare_capacity_mut()[..allowed],
            FlushDecompress::NoFlush,
        );
        let consumed = (self.codec.total_in() - before_in) as usize;
        let produced = (self.codec.total_out() - before_out) as usize;
        self.steps += 1;
        assert!(consumed <= supplied && produced <= allowed);
        // SAFETY: zlib-rs initialized exactly produced elements within the
        // supplied spare-capacity slice, also when the codec reports error.
        unsafe {
            self.output.set_len(self.output.len() + produced);
        }
        if self.output.len() > self.limit {
            return Err(Fault::DecodedLimit);
        }
        if self.output.len() > self.encoded.saturating_mul(256).saturating_add(1024 * 1024) {
            return Err(Fault::ExpansionLimit);
        }
        let ended = result.map_err(|_| Fault::Corrupt)? == InflateStatus::StreamEnd;
        if checking_end && (produced != 0 || ended && consumed != bytes.len()) {
            return Err(Fault::Corrupt);
        }
        if ended {
            if !checking_end || !self.no_takeover {
                Self::save_history(&mut self.history, &self.output[self.history_position..]);
            }
            self.history_position = self.output.len();
            self.codec.reset(false);
            if (!checking_end || !self.no_takeover) && !self.history.is_empty() {
                self.codec
                    .set_dictionary(self.history.make_contiguous())
                    .map_err(|_| Fault::Corrupt)?;
            }
        } else if consumed == 0 && produced == 0 && !bytes.is_empty() {
            return Err(Fault::Corrupt);
        }
        Ok(Step {
            consumed,
            produced,
            full: produced == allowed,
            ended,
        })
    }

    #[cfg(test)]
    pub fn steps(&self) -> usize {
        self.steps
    }
    #[cfg(test)]
    pub fn retained_capacity(&self) -> usize {
        self.output.capacity()
    }
}

pub struct InflateJob<'a> {
    state: &'a mut Inflater,
    bytes: &'a [u8],
    fin: bool,
    phase: u8,
    cursor: usize,
    ends: usize,
    finished: bool,
}

impl std::future::Future for InflateJob<'_> {
    type Output = Result<Option<Vec<u8>>, Fault>;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        use std::task::Poll;
        let this = self.get_mut();
        assert!(!this.finished, "completed deflate fragment polled again");
        let mut input_budget = CHUNK;
        let mut output_budget = CHUNK;
        let mut calls = 0;
        loop {
            let bytes = match this.phase {
                0 => this.bytes,
                1 => TAIL,
                2 => END,
                _ => unreachable!(),
            };
            if output_budget == 0 || input_budget == 0 && this.cursor < bytes.len() || calls == 8 {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            let step = match this.state.step(
                &bytes[this.cursor..],
                input_budget,
                output_budget,
                this.phase == 2,
            ) {
                Ok(step) => step,
                Err(error) => {
                    this.finished = true;
                    return Poll::Ready(Err(error));
                }
            };
            this.cursor += step.consumed;
            input_budget -= step.consumed;
            output_budget -= step.produced;
            calls += 1;
            if this.phase == 2 && step.ended {
                this.ends += 1;
            }
            if this.cursor == bytes.len() && !step.full {
                if this.phase == 0 && !this.fin {
                    this.finished = true;
                    return Poll::Ready(Ok(None));
                }
                if this.phase == 2 {
                    this.finished = true;
                    if this.ends != 1 {
                        return Poll::Ready(Err(Fault::Corrupt));
                    }
                    this.state.history_position = 0;
                    this.state.encoded = 0;
                    if this.state.no_takeover {
                        this.state.history.clear();
                    }
                    return Poll::Ready(Ok(Some(std::mem::take(&mut this.state.output))));
                }
                this.phase += 1;
                this.cursor = 0;
            }
        }
    }
}

pub struct Deflater {
    codec: Compress,
    no_takeover: bool,
}

impl Deflater {
    pub fn new(no_takeover: bool) -> Self {
        Self {
            codec: Compress::new(Compression::fast(), false),
            no_takeover,
        }
    }

    #[cfg(test)]
    pub fn message(&mut self, bytes: &[u8], limit: usize) -> Result<Vec<u8>, Fault> {
        use std::{
            future::Future,
            pin::Pin,
            task::{Context, Poll, Waker},
        };
        let mut job = self.message_job(bytes, limit)?;
        let mut context = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(result) = Pin::new(&mut job).poll(&mut context) {
                return result;
            }
        }
    }

    pub fn message_job<'a>(
        &'a mut self,
        bytes: &'a [u8],
        limit: usize,
    ) -> Result<DeflateJob<'a>, Fault> {
        if bytes.len() > limit {
            return Err(Fault::DecodedLimit);
        }
        Ok(DeflateJob {
            state: self,
            bytes,
            cursor: 0,
            output: Vec::new(),
            limit: encoded_limit(limit),
            finished: false,
        })
    }
}

pub struct DeflateJob<'a> {
    state: &'a mut Deflater,
    bytes: &'a [u8],
    cursor: usize,
    output: Vec<u8>,
    limit: usize,
    finished: bool,
}

impl std::future::Future for DeflateJob<'_> {
    type Output = Result<Vec<u8>, Fault>;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        use std::task::Poll;
        let this = self.get_mut();
        assert!(!this.finished, "completed deflate send polled again");
        let mut input_budget = CHUNK;
        let mut output_budget = CHUNK;
        for _ in 0..8 {
            if output_budget == 0 || input_budget == 0 && this.cursor < this.bytes.len() {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            let allowed = output_budget.min(this.limit.saturating_sub(this.output.len()));
            if allowed == 0 {
                this.finished = true;
                return Poll::Ready(Err(Fault::EncodedLimit));
            }
            let needed = this.output.len() + allowed;
            if this.output.capacity() < needed {
                let target = this
                    .output
                    .capacity()
                    .max(1024)
                    .saturating_mul(2)
                    .max(needed)
                    .min(this.limit);
                if this
                    .output
                    .try_reserve_exact(target - this.output.len())
                    .is_err()
                {
                    this.finished = true;
                    return Poll::Ready(Err(Fault::Allocation));
                }
            }
            let supplied = (this.bytes.len() - this.cursor).min(input_budget);
            let flush = if this.cursor + supplied == this.bytes.len() {
                FlushCompress::Sync
            } else {
                FlushCompress::None
            };
            let before_in = this.state.codec.total_in();
            let before_out = this.state.codec.total_out();
            let result = this.state.codec.compress_uninit(
                &this.bytes[this.cursor..this.cursor + supplied],
                &mut this.output.spare_capacity_mut()[..allowed],
                flush,
            );
            let consumed = (this.state.codec.total_in() - before_in) as usize;
            let produced = (this.state.codec.total_out() - before_out) as usize;
            assert!(consumed <= supplied && produced <= allowed);
            // SAFETY: flate2 initialized exactly produced bytes inside the
            // allowed-element spare-capacity slice, including an error return.
            unsafe {
                this.output.set_len(this.output.len() + produced);
            }
            this.cursor += consumed;
            input_budget -= consumed;
            output_budget -= produced;
            let result = match result {
                Ok(status) => status,
                Err(_) => {
                    this.finished = true;
                    return Poll::Ready(Err(Fault::Corrupt));
                }
            };
            if this.cursor == this.bytes.len() && produced < allowed && this.output.ends_with(TAIL)
            {
                this.output.truncate(this.output.len() - TAIL.len());
                if this.state.no_takeover {
                    this.state.codec.reset();
                }
                this.finished = true;
                return Poll::Ready(Ok(std::mem::take(&mut this.output)));
            }
            if result == Status::StreamEnd || consumed == 0 && produced == 0 {
                this.finished = true;
                return Poll::Ready(Err(Fault::Corrupt));
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// Room for literal coding and block headers, independent of decoded size.
pub fn encoded_limit(decoded: usize) -> usize {
    decoded.saturating_add(decoded / 8).saturating_add(CHUNK)
}

#[cfg(test)]
#[path = "websocket_deflate_tests.rs"]
mod tests;
