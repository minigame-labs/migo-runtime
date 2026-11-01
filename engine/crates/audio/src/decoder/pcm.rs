//! Collect decoded PCM without growing and copying the complete prefix.
//! Unknown lengths use pages; declared lengths reserve once, then fill in steps.
use std::collections::VecDeque;

use shared::error::{EngineError, EngineResult, ErrorCode};

const PAGE_SAMPLES: usize = 16_384;
const STEP_SAMPLES: usize = 8192;

enum Phase {
    Collecting {
        pages: VecDeque<Vec<f32>>,
        samples: usize,
        expected: Option<usize>,
        reserved: bool,
    },
    Flattening {
        pages: VecDeque<Vec<f32>>,
        head: usize,
        output: Vec<f32>,
        samples: usize,
    },
    Complete(Vec<f32>),
    Failed,
}

pub(crate) struct PcmCollector {
    phase: Phase,
}

impl PcmCollector {
    pub(crate) fn new() -> Self {
        Self::with_expected_samples(None)
    }

    /// A declaration is validated and reserved by a separate `step`, never here.
    pub(crate) fn with_expected_samples(expected: Option<usize>) -> Self {
        Self {
            phase: Phase::Collecting {
                pages: VecDeque::new(),
                samples: 0,
                expected,
                reserved: expected.is_none(),
            },
        }
    }

    /// Reservation is distinct from copying PCM, including an empty declaration.
    pub(crate) fn needs_reservation(&self) -> bool {
        matches!(
            self.phase,
            Phase::Collecting {
                reserved: false,
                ..
            }
        )
    }

    /// Admit the whole slice before allocating or appending any of it.
    pub(crate) fn append_i16(&mut self, pcm: &[i16]) -> EngineResult<()> {
        self.append_with(pcm, |sample| f32::from(sample) / 32768.0)
    }

    /// Fill a declared allocation directly with a mature codec's conversion.
    /// Unknown lengths return false without calling `write` or changing state.
    pub(crate) fn append_direct(
        &mut self,
        count: usize,
        write: impl FnOnce(&mut [f32]),
    ) -> EngineResult<bool> {
        let Phase::Collecting {
            pages,
            samples,
            expected: Some(expected),
            reserved,
        } = &mut self.phase
        else {
            return Ok(false);
        };
        if !*reserved {
            return Err(internal("PCM reservation requires its own step"));
        }
        let needed = samples.checked_add(count);
        if count > STEP_SAMPLES
            || needed.is_none_or(|n| n > *expected || !crate::limits::pcm_samples_within_budget(n))
        {
            self.phase = Phase::Failed;
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "decoded PCM exceeds its collection budget",
            ));
        }
        if count != 0 {
            let output = pages
                .front_mut()
                .ok_or_else(|| internal("declared PCM storage missing"))?;
            let start = output.len();
            output.resize(needed.unwrap(), 0.);
            write(&mut output[start..]);
        }
        *samples = needed.unwrap();
        Ok(true)
    }

    /// Shared bounded collection for codec-native samples and normalized f32.
    pub(crate) fn append_with<T: Copy>(
        &mut self,
        pcm: &[T],
        convert: impl Fn(T) -> f32 + Copy,
    ) -> EngineResult<()> {
        if self.needs_reservation() {
            return Err(internal("PCM reservation requires its own step"));
        }
        let Phase::Collecting {
            pages,
            samples,
            expected,
            ..
        } = &mut self.phase
        else {
            return Err(internal("PCM collection is already sealed"));
        };
        let needed = samples.checked_add(pcm.len());
        if pcm.len() > STEP_SAMPLES
            || !needed.is_some_and(crate::limits::pcm_samples_within_budget)
            || expected.is_some_and(|limit| needed.is_none_or(|count| count > limit))
        {
            self.phase = Phase::Failed;
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "decoded PCM exceeds its collection budget",
            ));
        }
        let page_limit = expected.unwrap_or(PAGE_SAMPLES);
        let mut remaining = pcm;
        while !remaining.is_empty() {
            if pages.back().is_none_or(|page| page.len() == page_limit) {
                let reservation = (|| {
                    pages.try_reserve(1)?;
                    let mut page = Vec::new();
                    page.try_reserve_exact(PAGE_SAMPLES)?;
                    Ok::<_, std::collections::TryReserveError>(page)
                })();
                match reservation {
                    Ok(page) => pages.push_back(page),
                    Err(_) => {
                        self.phase = Phase::Failed;
                        return Err(EngineError::from_detail(
                            ErrorCode::OutOfMemory,
                            "PCM page reservation refused",
                        ));
                    }
                }
            }
            let page = pages.back_mut().unwrap();
            let count = (page_limit - page.len()).min(remaining.len());
            page.extend(remaining[..count].iter().copied().map(convert));
            remaining = &remaining[count..];
        }
        *samples = needed.unwrap();
        Ok(())
    }

    /// Seal only complete PCM. A declared result moves its existing allocation;
    /// unknown lengths reserve their final size for bounded copying.
    pub(crate) fn seal(&mut self) -> EngineResult<()> {
        let phase = std::mem::replace(&mut self.phase, Phase::Failed);
        match phase {
            Phase::Collecting {
                mut pages,
                samples,
                expected: Some(expected),
                reserved,
            } => {
                if !reserved || samples != expected {
                    return Err(EngineError::from_detail(
                        ErrorCode::InvalidArgument,
                        "PCM does not match its declared length",
                    ));
                }
                let output = pages.pop_front().unwrap_or_default();
                if output.len() != samples || !pages.is_empty() {
                    return Err(internal("declared PCM storage lost samples"));
                }
                self.phase = Phase::Complete(output);
                Ok(())
            }
            Phase::Collecting { pages, samples, .. } => {
                self.phase = if samples == 0 {
                    Phase::Complete(Vec::new())
                } else {
                    let mut output = Vec::new();
                    output.try_reserve_exact(samples).map_err(|_| {
                        EngineError::from_detail(
                            ErrorCode::OutOfMemory,
                            "PCM final reservation refused",
                        )
                    })?;
                    Phase::Flattening {
                        pages,
                        head: 0,
                        output,
                        samples,
                    }
                };
                Ok(())
            }
            other => {
                self.phase = other;
                Err(internal("PCM collection cannot be sealed twice"))
            }
        }
    }

    /// Reserve a declaration without initializing samples, or copy at most
    /// STEP_SAMPLES. At most two unknown-length pages retire per copy step.
    pub(crate) fn step(&mut self) -> EngineResult<bool> {
        if self.needs_reservation() {
            let Phase::Collecting {
                pages,
                expected: Some(expected),
                reserved,
                ..
            } = &mut self.phase
            else {
                unreachable!()
            };
            if !crate::limits::pcm_samples_within_budget(*expected) {
                self.phase = Phase::Failed;
                return Err(EngineError::from_detail(
                    ErrorCode::InvalidArgument,
                    "declared PCM exceeds the collection budget",
                ));
            }
            let reservation = (|| {
                if *expected != 0 {
                    pages.try_reserve_exact(1)?;
                    let mut output = Vec::new();
                    output.try_reserve_exact(*expected)?;
                    pages.push_back(output);
                }
                Ok::<_, std::collections::TryReserveError>(())
            })();
            if reservation.is_err() {
                self.phase = Phase::Failed;
                return Err(EngineError::from_detail(
                    ErrorCode::OutOfMemory,
                    "PCM reservation refused",
                ));
            }
            *reserved = true;
            return Ok(false);
        }
        match &mut self.phase {
            Phase::Complete(_) => Ok(true),
            Phase::Flattening {
                pages,
                head,
                output,
                samples,
            } => {
                let mut remaining = STEP_SAMPLES;
                while remaining != 0 {
                    let Some(page) = pages.front() else {
                        break;
                    };
                    let count = (page.len() - *head).min(remaining);
                    output.extend_from_slice(&page[*head..*head + count]);
                    *head += count;
                    remaining -= count;
                    if *head == page.len() {
                        pages.pop_front();
                        *head = 0;
                    }
                }
                if pages.is_empty() {
                    if output.len() != *samples {
                        self.phase = Phase::Failed;
                        return Err(internal("PCM finalization lost samples"));
                    }
                    self.phase = Phase::Complete(std::mem::take(output));
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Phase::Collecting { .. } => Err(internal("PCM collection is not sealed")),
            Phase::Failed => Err(internal("PCM collection already failed")),
        }
    }

    pub(crate) fn finish(self) -> EngineResult<Vec<f32>> {
        match self.phase {
            Phase::Complete(output) => Ok(output),
            _ => Err(internal("PCM collection has not finished")),
        }
    }
}

fn internal(detail: &'static str) -> EngineError {
    EngineError::from_detail(ErrorCode::Internal, detail)
}

#[cfg(test)]
#[path = "pcm_tests.rs"]
mod tests;
