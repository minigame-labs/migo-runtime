//! Optional duration-only pass through the same mature Vorbis packetizer.
//! Startup/total budgets fall back to the normal paged reader from input start.
use super::*;
pub(super) enum Outcome {
    Yield,
    Fallback,
    Complete(usize),
}
pub(super) struct Scan {
    parser: VorbisPacketizer,
    offset: usize,
    window: usize,
    frames: u64,
    pub(super) calls: usize,
}
impl Scan {
    pub(super) fn new() -> Self {
        Self {
            parser: VorbisPacketizer::new(PacketizerOptions::default()),
            offset: 0,
            window: INPUT_BYTES,
            frames: 0,
            calls: 0,
        }
    }
    pub(super) fn step(&mut self, data: &[u8], limit: usize) -> EngineResult<Outcome> {
        let end = (self.offset + self.window).min(data.len());
        if self.calls >= 4096 || (self.frames == 0 && (end > 65536 || self.calls >= 128)) {
            return Ok(Outcome::Fallback);
        }
        self.calls += 1;
        let input = &data[self.offset..end];
        let eof = end == data.len();
        #[cfg(test)]
        TRACE.with(|t| {
            let mut t = t.borrow_mut();
            t.parser_calls += 1;
            t.input_view = input.len();
        });
        let (consumed, ended) = match self.parser.next(input, eof).map_err(codec_error)? {
            PacketStep::NeedMore => {
                if eof || self.window == MAX_WINDOW {
                    return Err(invalid("OGG duration exceeded its input window"));
                }
                self.window = (self.window + INPUT_BYTES).min(MAX_WINDOW);
                return Ok(Outcome::Yield);
            }
            PacketStep::Discard(n) => (n, false),
            PacketStep::Packet { consumed } => {
                let packet = self
                    .parser
                    .packet(input)
                    .ok_or_else(|| invalid("OGG duration packet missing"))?;
                self.frames = self
                    .frames
                    .checked_add(packet.dur.get())
                    .ok_or_else(|| invalid("OGG duration overflow"))?;
                (consumed, false)
            }
            PacketStep::End => (0, true),
        };
        if consumed > input.len() {
            return Err(invalid("OGG duration consumed beyond input"));
        }
        self.offset += consumed;
        if consumed != 0 {
            self.window = INPUT_BYTES;
        }
        let Some(params) = self
            .parser
            .track()
            .and_then(|t| t.codec_params.as_ref())
            .and_then(|p| p.audio())
        else {
            if ended {
                return Err(invalid("OGG duration has no audio parameters"));
            }
            return Ok(Outcome::Yield);
        };
        let (_, channels) = format(params)?;
        let samples = self
            .frames
            .checked_mul(u64::from(channels))
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n <= limit)
            .ok_or_else(|| invalid("OGG duration exceeds PCM budget"))?;
        if ended {
            if self.offset != data.len() || samples == 0 {
                return Err(invalid("OGG duration has no complete playable audio"));
            }
            Ok(Outcome::Complete(samples))
        } else {
            Ok(Outcome::Yield)
        }
    }
}
