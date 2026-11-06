//! Retained sealed-source replay over the existing native codec and SRC grid.
use super::*;
use crate::stream_control::LoopRequest;
use crate::streaming::{StreamCutover, StreamPresentation};
use symphonia_core::errors::{Error, SeekErrorKind};
impl Window {
    pub(super) fn repeats(&self) -> bool {
        self.cycles
            .is_none_or(|n| self.completed.checked_add(1).is_some_and(|next| next < n))
    }
}
impl Owner {
    pub(super) fn restart_codec(&mut self, cached: bool) {
        self.frames.invalidate_header();
        self.probe.invalidate_header();
        self.presentation = None;
        if !cached {
            self.buffer.clear();
            self.encoded_origin = 0;
        }
        self.encoded_head = 0;
        self.scratch.clear();
        self.head = 0;
        self.pending_dsp = false;
        self.eof = cached;
        self.dsp_finished = false;
        self.want_input = true;
        self.previous = None;
        self.flush = Flush::Search;
        self.candidates = 0;
        self.started = false;
        self.validated = false;
        self.tag_remaining = 0;
    }
}
impl Mp3StreamDecoder {
    pub(crate) fn retained(target: u32) -> Self {
        Self {
            retained: true,
            ..Self::new(target)
        }
    }
    pub(crate) fn source_request(&self) -> Option<u64> {
        self.owner.as_ref()?.source_pending.then_some(0)
    }
    pub(crate) fn resume_at(&mut self, offset: u64) -> EngineResult<()> {
        if self.source_request() != Some(offset) {
            return Err(invalid("MP3 replay source request changed"));
        }
        self.owner.as_mut().unwrap().source_pending = false;
        Ok(())
    }
    pub(crate) fn reposition(
        &mut self,
        target: i64,
        cutover: Option<LoopRequest>,
    ) -> Result<(Option<u64>, u64, bool), Error> {
        let reject = || Error::SeekError(SeekErrorKind::OutOfRange);
        let unavailable = || Error::SeekError(SeekErrorKind::Unseekable);
        if !self.retained || self.error.is_some() {
            return Err(unavailable());
        }
        let clock = self.clock.ok_or_else(unavailable)?;
        let owner = self.owner.as_mut().ok_or_else(unavailable)?;
        let frames = owner.media_frames.ok_or_else(unavailable)?;
        let history = owner.history.ok_or_else(unavailable)?;
        let (origin, start, cycles, presentation) = if let Some(request) =
            cutover.filter(|r| r.seek_target.is_none())
        {
            let p = StreamCutover::new(clock, request.output_start, request.enabled, history)
                .map_err(|_| reject())?;
            (
                p.input_origin(),
                p.output_start(),
                p.cycles(),
                p.presentation(),
            )
        } else {
            let frame = u64::try_from(target).map_err(|_| reject())?;
            let position = clock.position(frame).map_err(|_| reject())?;
            let start = position.frame + u64::from(position.phase != 0);
            if cutover.is_some_and(|r| r.seek_target != Some(frame) || r.output_start != start) {
                return Err(reject());
            }
            let enabled = cutover.is_some_and(|r| r.enabled);
            let p = if enabled {
                StreamPresentation::repeated(clock, None).map_err(|_| reject())?
            } else {
                StreamPresentation::single(clock)
            };
            (0, start, (!enabled).then_some(1), p)
        };
        let origin = origin / owner.grid * owner.grid;
        let end = cycles
            .map(|n| n.checked_mul(frames).ok_or_else(reject))
            .transpose()?;
        let pipeline = Stream::new_at(
            owner.source_format.0,
            owner.target,
            owner.source_format.1,
            end,
            origin,
            start,
        )
        .map_err(|_| reject())?;
        let terminal = presentation.output_end() == Some(start);
        // All recoverable refusal checks precede replacement on the original CPU.
        let cached = owner.validated && owner.eof && owner.encoded_origin == 0;
        owner.restart_codec(cached);
        owner.output.clear();
        owner.pipeline = Some(pipeline);
        owner.window = Some(Window {
            frames,
            local: 0,
            completed: origin / frames,
            cycles,
            skip: origin % frames,
            presentation,
        });
        owner.dsp_finished = terminal;
        owner.source_pending = !terminal && !cached;
        Ok((owner.source_pending.then_some(0), start, terminal))
    }
}

#[cfg(test)]
#[path = "streaming_mp3_control_tests.rs"]
mod tests;
