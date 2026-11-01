//! Bounded duration-only pass through the existing library packetizers.
//! No codec decode, container parser or alternate timing algorithm lives here.
use super::*;
use symphonia::core::formats::Track;
use symphonia_format_ogg::streaming::{PacketStep as OggStep, PacketizerOptions, VorbisPacketizer};

enum Parser {
    Vorbis(Box<VorbisPacketizer>),
    #[cfg(feature = "adts-streaming-probe")]
    Adts(Box<symphonia_codec_aac::streaming::AdtsPacketizer>),
}
pub(super) enum Outcome {
    Yield,
    Complete { samples: usize, channels: u32 },
    Fallback,
}
// Abandon optional preflight before large headers or many tiny startup steps.
// The ordinary codec still validates the complete input on this path.
const STARTUP_BYTES: usize = 65536;
const STARTUP_STEPS: u32 = 128;
const SCAN_STEPS: u32 = 4096;
struct Event {
    consumed: usize,
    frames: u64,
    need_more: bool,
    ended: bool,
}
pub(super) struct Scan {
    parser: Parser,
    offset: usize,
    window: usize,
    frames: u64,
    calls: u32,
}
impl Scan {
    pub(super) fn new(format: Format) -> Option<Self> {
        let parser = match format {
            Format::Vorbis => Parser::Vorbis(Box::new(VorbisPacketizer::new(
                PacketizerOptions::default(),
            ))),
            #[cfg(feature = "adts-streaming-probe")]
            Format::Adts => Parser::Adts(Box::new(
                symphonia_codec_aac::streaming::AdtsPacketizer::new(Default::default()),
            )),
            _ => return None,
        };
        Some(Self {
            parser,
            offset: 0,
            window: INPUT_BYTES,
            frames: 0,
            calls: 0,
        })
    }
    fn track(&self) -> Option<&Track> {
        match &self.parser {
            Parser::Vorbis(parser) => parser.track(),
            #[cfg(feature = "adts-streaming-probe")]
            Parser::Adts(parser) => parser.track(),
        }
    }
    /// One existing library operation. Views grow by <=8192B between yields;
    /// a complete legal Ogg page may require <=65536B. No encoded copy occurs.
    pub(super) fn step(&mut self, data: &[u8], target: u32, limit: usize) -> EngineResult<Outcome> {
        let end = (self.offset + self.window).min(data.len());
        if self.calls >= SCAN_STEPS
            || (self.frames == 0 && (end > STARTUP_BYTES || self.calls >= STARTUP_STEPS))
        {
            return Ok(Outcome::Fallback);
        }
        self.calls += 1;
        let input = &data[self.offset..end];
        let eof = end == data.len();
        let event = match &mut self.parser {
            Parser::Vorbis(parser) => match parser
                .next(input, eof)
                .map_err(|_| invalid("duration scan rejected Vorbis framing"))?
            {
                OggStep::NeedMore => Event {
                    consumed: 0,
                    frames: 0,
                    need_more: true,
                    ended: false,
                },
                OggStep::Discard(consumed) => Event {
                    consumed,
                    frames: 0,
                    need_more: false,
                    ended: false,
                },
                OggStep::Packet { consumed } => Event {
                    consumed,
                    frames: parser
                        .packet(input)
                        .ok_or_else(|| invalid("duration scan lost its packet"))?
                        .dur
                        .get(),
                    need_more: false,
                    ended: false,
                },
                OggStep::End => Event {
                    consumed: 0,
                    frames: 0,
                    need_more: false,
                    ended: true,
                },
            },
            #[cfg(feature = "adts-streaming-probe")]
            Parser::Adts(parser) => {
                use symphonia_codec_aac::streaming::PacketStep;
                match parser
                    .next(input, eof)
                    .map_err(|_| invalid("duration scan rejected ADTS framing"))?
                {
                    PacketStep::NeedMore => Event {
                        consumed: 0,
                        frames: 0,
                        need_more: true,
                        ended: false,
                    },
                    PacketStep::Discard(consumed) => Event {
                        consumed,
                        frames: 0,
                        need_more: false,
                        ended: false,
                    },
                    PacketStep::Packet { consumed, packet } => Event {
                        consumed,
                        frames: packet.dur.get(),
                        need_more: false,
                        ended: false,
                    },
                    PacketStep::End => Event {
                        consumed: 0,
                        frames: 0,
                        need_more: false,
                        ended: true,
                    },
                }
            }
        };
        if event.consumed > input.len() {
            return Err(invalid("duration scanner consumed beyond input"));
        }
        if event.need_more {
            let max = match self.parser {
                Parser::Vorbis(_) => 65536,
                #[cfg(feature = "adts-streaming-probe")]
                Parser::Adts(_) => INPUT_BYTES,
            };
            if eof || self.window == max {
                return Err(invalid("duration scan exceeded its input window"));
            }
            self.window = (self.window + INPUT_BYTES).min(max);
            return Ok(Outcome::Yield);
        }
        self.offset += event.consumed;
        if event.consumed != 0 {
            self.window = INPUT_BYTES;
        }
        self.frames = self
            .frames
            .checked_add(event.frames)
            .ok_or_else(|| invalid("duration scan frame count overflow"))?;
        let Some(track) = self.track() else {
            if event.ended {
                return Err(invalid("duration scan has no audio track"));
            }
            return Ok(Outcome::Yield);
        };
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| invalid("duration scan has no audio parameters"))?;
        let rate = params
            .sample_rate
            .ok_or_else(|| invalid("duration scan has no source rate"))?;
        let channels = params
            .channels
            .as_ref()
            .ok_or_else(|| invalid("duration scan has no channels"))?
            .count() as u32;
        crate::bandlimited::validate_format(rate, target, channels)
            .map_err(|_| invalid("duration scan format outside limits"))?;
        if self.frames == 0 {
            if event.ended {
                return Err(invalid("duration scan has no playable frames"));
            }
            return Ok(Outcome::Yield);
        }
        let timeline = crate::streaming::StreamTimeline::new(self.frames, rate, target, channels)?;
        let samples = timeline
            .output_frames()
            .checked_mul(u64::from(channels))
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n <= limit)
            .ok_or_else(|| invalid("duration scan exceeds output quota"))?;
        if event.ended {
            if self.offset != data.len() || self.frames == 0 {
                return Err(invalid("duration scan did not finish complete audio"));
            }
            Ok(Outcome::Complete { samples, channels })
        } else {
            Ok(Outcome::Yield)
        }
    }
}

#[cfg(test)]
#[path = "whole_file_duration_contracts.rs"]
mod tests;
