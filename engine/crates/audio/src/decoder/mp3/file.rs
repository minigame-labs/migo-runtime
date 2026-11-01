//! Bounded whole-file framing around the same persistent minimp3 decoder.
//! The caller owns PCM collection; this cursor yields at most one borrowed frame.
use std::sync::atomic::{AtomicBool, Ordering};

use shared::error::{EngineError, EngineResult, ErrorCode};

use super::{FrameBoundary, Mp3FrameDecoder, Mp3Step, id3v2_tag_length, next_sync_candidate};

const SCAN_BYTES: usize = 4096;
const NATIVE_BYTES: usize = 8192;
const NATIVE_CALLS: usize = 4;
// Includes free-format frames, whose size minimp3 derives from later headers.
// Mirrors minimp3's MAX_FREE_FORMAT_FRAME_SIZE, not a bitrate/header table.
const FRAME_BYTES: usize = 2304;
const TOTAL_CALLS: usize = 131_072;

enum Phase {
    Start,
    Search,
    Probe {
        hint: bool,
        lookahead: u8,
        length: usize,
    },
    Presentation(FrameBoundary),
    Decode(FrameBoundary),
    Fallback,
    End,
}

struct State<B> {
    input: B,
    frames: Mp3FrameDecoder,
    probe: Mp3FrameDecoder,
    position: usize,
    gap_start: usize,
    failed: usize,
    previous: Option<usize>,
    phase: Phase,
    calls_left: usize,
    presentation: Option<super::presentation::Presentation>,
}

pub(crate) enum FileStep<'a> {
    Frame {
        pcm: &'a [i16],
        sample_rate: u32,
        channels: u32,
    },
    Yield,
    Finished,
}

pub(crate) struct Mp3FileDecoder<B> {
    state: Option<State<B>>,
    failure: Option<ErrorCode>,
}

impl<B: AsRef<[u8]>> Mp3FileDecoder<B> {
    pub(crate) fn new(input: B) -> Self {
        Self {
            state: Some(State {
                input,
                frames: Mp3FrameDecoder::new(),
                probe: Mp3FrameDecoder::new(),
                position: 0,
                gap_start: 0,
                failed: 0,
                previous: None,
                phase: Phase::Start,
                calls_left: TOTAL_CALLS,
                presentation: None,
            }),
            failure: None,
        }
    }

    pub(crate) fn presentation_samples(&self) -> Option<Option<usize>> {
        self.state
            .as_ref()?
            .presentation
            .as_ref()
            .map(|owner| owner.expected)
    }

    pub(crate) fn step(&mut self, cancel: &AtomicBool) -> EngineResult<FileStep<'_>> {
        if cancel.load(Ordering::Acquire) {
            self.failure = Some(ErrorCode::Cancelled);
        }
        if let Some(code) = self.failure {
            self.state = None;
            return Err(EngineError::from_detail(code, "MP3 decode retired"));
        }
        if self
            .state
            .as_ref()
            .is_some_and(|state| matches!(state.phase, Phase::End))
        {
            let result = if let Some(state) = self.state.as_mut() {
                if let Some(owner) = &state.presentation {
                    if let Some(next) = state.calls_left.checked_sub(1) {
                        state.calls_left = next;
                        owner.finish()
                    } else {
                        Err(invalid("MP3 native-call budget exceeded"))
                    }
                } else {
                    Ok(())
                }
            } else {
                Ok(())
            };
            self.state = None;
            if let Err(error) = result {
                self.failure = Some(error.code);
                return Err(error);
            }
        }
        let Some(state) = self.state.as_mut() else {
            return Ok(FileStep::Finished);
        };
        let data = state.input.as_ref();
        let mut scans = SCAN_BYTES;
        let mut calls = NATIVE_CALLS;
        let mut bytes = NATIVE_BYTES;
        loop {
            match &mut state.phase {
                Phase::Start => {
                    if scans < 10 {
                        return Ok(FileStep::Yield);
                    }
                    scans -= 10;
                    if let Some(length) = id3v2_tag_length(&data[state.position..]) {
                        state.position += length;
                        continue;
                    }
                    state.gap_start = state.position;
                    state.failed = 0;
                    state.phase = Phase::Search;
                }
                Phase::Search => {
                    if data.len() - state.position < 4 {
                        state.phase = Phase::End;
                        return Ok(FileStep::Yield);
                    }
                    if scans < 2 {
                        return Ok(FileStep::Yield);
                    }
                    let end = (state.position + scans).min(data.len());
                    let window = &data[state.position..end];
                    scans -= window.len();
                    if let Some(offset) = next_sync_candidate(window, 0) {
                        state.position += offset;
                        state.phase = Phase::Probe {
                            hint: true,
                            lookahead: 0,
                            length: 4,
                        };
                    } else if end == data.len() {
                        state.phase = Phase::End;
                        return Ok(FileStep::Yield);
                    } else {
                        // Keep the last byte for a sync word straddling windows.
                        state.position = end - 1;
                        return Ok(FileStep::Yield);
                    }
                }
                Phase::Probe {
                    hint,
                    lookahead,
                    length,
                } => {
                    if state.probe.has_free_format_state() && !state.frames.has_free_format_state()
                    {
                        // A cached free-format probe can accept a view a fresh
                        // real decoder cannot. Re-measure through the native
                        // initialization path after a damaged frame reset.
                        state.probe.invalidate_header();
                    }
                    let remaining = data.len() - state.position;
                    let (size, exact) = if *hint {
                        match state.previous {
                            Some(size) if size <= remaining => (size, true),
                            _ => {
                                *hint = false;
                                continue;
                            }
                        }
                    } else if *lookahead < 2 {
                        let limit = if *lookahead == 0 {
                            2 * super::MAX_FRAME_BYTES + 4
                        } else {
                            NATIVE_BYTES
                        };
                        (remaining.min(limit), false)
                    } else if *length <= remaining.min(FRAME_BYTES) {
                        (*length, true)
                    } else {
                        state.failed += 1;
                        if state.failed >= super::MAX_SYNC_CANDIDATES {
                            state.position = state.gap_start;
                            state.phase = Phase::Fallback;
                        } else {
                            state.position += 1;
                            state.phase = Phase::Search;
                        }
                        continue;
                    };
                    if !admit_native(&mut state.calls_left, &mut calls, &mut bytes, size)? {
                        return Ok(FileStep::Yield);
                    }
                    if *hint {
                        *hint = false;
                    } else if *lookahead < 2 {
                        *lookahead += 1;
                    } else {
                        *length += 1;
                    }
                    if let Some(frame) = state
                        .probe
                        .measured_frame(&data[state.position..state.position + size])
                        && (!exact || frame.offset + frame.length == size)
                    {
                        if state
                            .presentation
                            .as_ref()
                            .is_some_and(|owner| !owner.accepts(&frame.info))
                        {
                            self.failure = Some(ErrorCode::InvalidArgument);
                            return Err(invalid("MP3 source format changed"));
                        }
                        state.position += frame.offset;
                        state.previous = Some(frame.length);
                        let frame = FrameBoundary { offset: 0, ..frame };
                        if state.presentation.is_none() {
                            state.phase = Phase::Presentation(frame);
                            return Ok(FileStep::Yield);
                        }
                        state.phase = Phase::Decode(frame);
                    }
                }
                Phase::Presentation(frame) => {
                    if calls < 2
                        || !admit_native(
                            &mut state.calls_left,
                            &mut calls,
                            &mut bytes,
                            frame.length,
                        )?
                    {
                        return Ok(FileStep::Yield);
                    }
                    admit_native(&mut state.calls_left, &mut calls, &mut bytes, 0)?;
                    let result = super::presentation::Presentation::new(
                        &data[state.position..state.position + frame.length],
                        frame,
                    );
                    let (owner, skip_tag) = match result {
                        Ok(value) => value,
                        Err(error) => {
                            self.failure = Some(error.code);
                            return Err(error);
                        }
                    };
                    state.presentation = Some(owner);
                    state.phase = if skip_tag {
                        state.position += frame.length;
                        Phase::Start
                    } else {
                        Phase::Decode(FrameBoundary {
                            offset: frame.offset,
                            length: frame.length,
                            free_format: frame.free_format,
                            view: frame.view,
                            info: frame.info,
                            free_format_bytes: frame.free_format_bytes,
                        })
                    };
                    return Ok(FileStep::Yield);
                }
                Phase::Decode(frame) => {
                    let size = if frame.free_format && !state.frames.has_free_format_state() {
                        // Reuse the exact successful probe view. A larger view
                        // may expose a trailing tag that defeats native sync.
                        frame.view
                    } else {
                        frame.length
                    };
                    if !admit_native(&mut state.calls_left, &mut calls, &mut bytes, size)? {
                        return Ok(FileStep::Yield);
                    }
                    let start = state.position;
                    state.position += frame.length;
                    state.phase = Phase::Start;
                    let decoded = state
                        .frames
                        .decode_presented(
                            &data[start..start + size],
                            state.presentation.as_mut().unwrap(),
                        )
                        .inspect_err(|error| {
                            self.failure = Some(error.code);
                        })?;
                    return Ok(match decoded {
                        Mp3Step::Frame {
                            pcm,
                            sample_rate,
                            channels,
                            consumed,
                        } => {
                            state.position = start + consumed;
                            FileStep::Frame {
                                pcm,
                                sample_rate,
                                channels,
                            }
                        }
                        Mp3Step::Skipped(consumed) => {
                            state.position = start + consumed;
                            FileStep::Yield
                        }
                        Mp3Step::NeedMoreData => FileStep::Yield,
                    });
                }
                Phase::Fallback => {
                    let size = (data.len() - state.position).min(NATIVE_BYTES);
                    if !admit_native(&mut state.calls_left, &mut calls, &mut bytes, size)? {
                        return Ok(FileStep::Yield);
                    }
                    let start = state.position;
                    state.phase = Phase::Start;
                    #[cfg(test)]
                    super::NATIVE_TRACE.with(|trace| {
                        let mut value = trace.get();
                        value.fallbacks += 1;
                        trace.set(value);
                    });
                    let decoded = if let Some(owner) = state.presentation.as_mut() {
                        state
                            .frames
                            .decode_presented(&data[start..start + size], owner)
                            .inspect_err(|error| {
                                self.failure = Some(error.code);
                            })?
                    } else {
                        let (boundary, info) = state.probe.measure(&data[start..start + size]);
                        if let Some(frame) = boundary {
                            state.position += frame.offset;
                            state.previous = Some(frame.length);
                            state.phase = Phase::Presentation(FrameBoundary { offset: 0, ..frame });
                            return Ok(FileStep::Yield);
                        }
                        state.probe.frame_step(size, info, 0, 0, false)
                    };
                    return Ok(match decoded {
                        Mp3Step::Frame {
                            pcm,
                            sample_rate,
                            channels,
                            consumed,
                        } => {
                            state.position += consumed;
                            FileStep::Frame {
                                pcm,
                                sample_rate,
                                channels,
                            }
                        }
                        Mp3Step::Skipped(consumed) => {
                            let retained = if start + size < data.len() {
                                2 * FRAME_BYTES + 4
                            } else {
                                0
                            };
                            state.position += consumed.min(size.saturating_sub(retained));
                            if state.position == start {
                                state.phase = Phase::End;
                            }
                            FileStep::Yield
                        }
                        Mp3Step::NeedMoreData => {
                            state.phase = Phase::End;
                            FileStep::Yield
                        }
                    });
                }
                Phase::End => return Ok(FileStep::Yield),
            }
        }
    }
}

fn admit_native(
    total: &mut usize,
    calls: &mut usize,
    bytes: &mut usize,
    size: usize,
) -> EngineResult<bool> {
    if *calls == 0 || size > *bytes {
        return Ok(false);
    }
    let next = total
        .checked_sub(1)
        .ok_or_else(|| invalid("MP3 native-call budget exceeded"))?;
    *total = next;
    *calls -= 1;
    *bytes -= size;
    Ok(true)
}

fn invalid(detail: &'static str) -> EngineError {
    EngineError::from_detail(ErrorCode::InvalidArgument, detail)
}

#[cfg(test)]
#[path = "file_tests.rs"]
pub(crate) mod tests;
