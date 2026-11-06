//! Typed WAVE/PCM binding; RIFF/fmt/fact/data parsing stays in Symphonia.
use crate::decoder::aac::incremental::{self, PacketStep, Packetizer};
use symphonia_core::{
    codecs::audio::{AudioCodecParameters, AudioDecoderOptions, well_known::*},
    errors::Result as FormatResult,
    formats::Track,
};
use symphonia_format_riff::streaming::{PacketStep as WavStep, PacketizerOptions, WavPacketizer};

pub struct Wave {
    packets: WavPacketizer,
    // Common Decoder counts a buffered optional tail as consumed when End
    // precedes source EOF. Keep those retained-window coordinates too.
    tail_in_window: usize,
}
impl Packetizer for Wave {
    type Codec = symphonia_codec_pcm::PcmDecoder;
    const MAX_TRAILING_BYTES: usize = 4 * 1024 * 1024;
    const MAX_DISCARDED_BYTES: Option<usize> = Some(4 * 1024 * 1024);
    // PCM conversion can consume a complete bounded input buffer. Optional
    // metadata still obeys the shared 4096-byte scanning budget.
    const ENCODED_BYTES_PER_STEP: usize = incremental::ENCODED_CAPACITY;
    fn new() -> Self {
        Self {
            packets: WavPacketizer::new(PacketizerOptions {
                max_format_bytes: incremental::ENCODED_CAPACITY,
                max_packet_bytes: incremental::SAMPLES_PER_STEP,
            }),
            tail_in_window: 0,
        }
    }
    fn track(&self) -> Option<&Track> {
        self.packets.track()
    }
    fn next<'a>(&mut self, input: &'a [u8], eof: bool) -> FormatResult<PacketStep<'a>> {
        self.packets.next(input, eof).map(|step| match step {
            WavStep::NeedMore => PacketStep::NeedMore,
            WavStep::Discard(n) => PacketStep::Discard(n),
            WavStep::Packet { consumed, packet } => PacketStep::Packet { consumed, packet },
            WavStep::ReadAt(_) => PacketStep::NeedSource,
            WavStep::End => {
                self.tail_in_window = if eof { 0 } else { input.len() };
                PacketStep::End
            }
        })
    }
    fn seek(
        &mut self,
        target: symphonia_core::units::Timestamp,
    ) -> FormatResult<symphonia_core::formats::SeekedTo> {
        self.packets.seek(target)
    }
    fn resume_buffered(&mut self, input: &[u8], head: usize) -> FormatResult<Option<usize>> {
        use symphonia_core::errors::Error;
        let Some(offset) = self.packets.source_request() else {
            return Ok(None);
        };
        let base = self
            .packets
            .position()
            .checked_add(self.tail_in_window as u64)
            .and_then(|n| n.checked_sub(head as u64))
            .ok_or(Error::DecodeError(
                "WAVE retained input coordinates changed",
            ))?;
        let Some(at) = offset
            .checked_sub(base)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n < input.len())
        else {
            return Ok(None);
        };
        self.packets.resume_at(offset)?;
        self.tail_in_window = 0;
        Ok(Some(at))
    }
    fn validate_codec(params: &AudioCodecParameters) -> Result<(), String> {
        if [
            CODEC_ID_PCM_U8,
            CODEC_ID_PCM_S16LE,
            CODEC_ID_PCM_S24LE,
            CODEC_ID_PCM_S32LE,
            CODEC_ID_PCM_F32LE,
        ]
        .contains(&params.codec)
        {
            Ok(())
        } else {
            Err("streaming WAVE profile supports integer PCM and float32".into())
        }
    }
    fn create_decoder(params: &AudioCodecParameters) -> Result<Self::Codec, String> {
        Self::Codec::try_new(params, &AudioDecoderOptions::default())
            .map_err(|e| format!("decoder: {e}"))
    }
}

impl super::packet::PacketStream<Wave> {
    pub(crate) fn wave(target: u32, length: u64) -> Self {
        let packets = Wave {
            packets: WavPacketizer::with_object_len(
                PacketizerOptions {
                    max_format_bytes: incremental::ENCODED_CAPACITY,
                    max_packet_bytes: incremental::SAMPLES_PER_STEP,
                },
                length,
            ),
            tail_in_window: 0,
        };
        let mut d =
            Self::with_decoder(target, incremental::Decoder::with_packetizer(packets, None));
        d.retain();
        d
    }
    pub(crate) fn wave_request(&self) -> Option<u64> {
        self.owner
            .as_ref()?
            .decoder
            .source_packetizer()?
            .packets
            .source_request()
    }
    pub(crate) fn resume_wave(&mut self, offset: u64) -> EngineResult<()> {
        let owner = self
            .owner
            .as_mut()
            .ok_or_else(|| super::invalid("WAVE decoder retired"))?;
        owner
            .decoder
            .resume_source(true, |p| {
                p.packets.resume_at(offset).map_err(|e| e.to_string())?;
                p.tail_in_window = 0;
                Ok(())
            })
            .map_err(super::invalid)?;
        owner.input_ended = false;
        Ok(())
    }
    pub(crate) fn reposition_wave(
        &mut self,
        target: i64,
        cutover: Option<crate::stream_control::LoopRequest>,
    ) -> FormatResult<(Option<u64>, u64, bool)> {
        let (start, terminal) = self.reposition_window(target, cutover)?;
        Ok((
            if terminal { None } else { self.wave_request() },
            start,
            terminal,
        ))
    }
}
use shared::error::EngineResult;

#[cfg(test)]
mod retirement {
    use super::*;
    use crate::off_worker::OffWorker;
    use crate::streaming_sinc::STEP_SAMPLES;
    use crate::streaming_transport::{Decoder, Progress};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    #[tokio::test]
    async fn wave_eof_retains_actual_capacities_until_original_cpu_really_destroys_them() {
        let host = migo_io::scheduler::IoScheduler::new(1061);
        let peer = migo_io::scheduler::IoScheduler::new(1062);
        let slot = OffWorker::new(
            None::<crate::streaming_codecs::packet::PacketStream<Wave>>,
            host.audio_cpu(),
        );
        let (slot, watches) = slot
            .with(|slot| {
                let mut data = crate::streaming_codecs::tests::wav(44100, 2, 9737);
                data.extend(b"LIST");
                data.extend(37u32.to_le_bytes());
                data.resize(data.len() + 38, 0);
                let length = (data.len() - 8) as u32;
                data[4..8].copy_from_slice(&length.to_le_bytes());
                let mut d =
                    crate::streaming_codecs::packet::PacketStream::wave(16000, data.len() as u64);
                let mut pos = 0;
                let mut source_eof = false;
                let mut out = Vec::with_capacity(STEP_SAMPLES);
                for _ in 0..10000 {
                    out.clear();
                    let end = (pos + 1024).min(data.len());
                    let step = d
                        .step(
                            &data[pos..end],
                            source_eof,
                            &mut out,
                            &AtomicBool::new(false),
                        )
                        .unwrap();
                    pos += step.consumed;
                    if step.progress == Progress::NeedInput && pos == data.len() {
                        source_eof = true;
                    }
                    assert_ne!(step.progress, Progress::NeedSource);
                    if step.progress == Progress::Finished {
                        break;
                    }
                }
                let o = d
                    .owner
                    .as_ref()
                    .expect("EOF dropped the retained source decoder");
                assert!(
                    o.decoder.input_len() > 0
                        && o.decoder.input_len() < incremental::ENCODED_CAPACITY,
                    "WAVE EOF lost actual retained capacity"
                );
                assert_eq!(
                    o.decoder.input_allocation().1,
                    incremental::ENCODED_CAPACITY
                );
                let (request, _, terminal) = d
                    .reposition_wave(0, None)
                    .expect("WAVE EOF restart did not preserve retained input coordinates");
                assert!(
                    request.is_none() && !terminal,
                    "retained WAVE EOF restart unexpectedly reread source"
                );
                let watches = d.watches();
                *slot = Some(d);
                watches
            })
            .await
            .unwrap();
        let bytes = watches.iter().map(|(_, n)| *n as u64).sum::<u64>();
        let (start, started) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let task = tokio::spawn(slot.with(move |slot| {
            start.send(()).unwrap();
            blocked
                .recv_timeout(std::time::Duration::from_secs(15))
                .unwrap();
            let mut out = Vec::with_capacity(STEP_SAMPLES);
            let before = migo_alloc_probe::thread_counts();
            assert_eq!(
                slot.as_mut()
                    .unwrap()
                    .step(&[], false, &mut out, &flag)
                    .unwrap()
                    .progress,
                Progress::Cancelled
            );
            assert!(
                migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed >= bytes,
                "original CPU did not really free retained replay buffers"
            );
            assert!(
                std::thread::current()
                    .name()
                    .unwrap_or("")
                    .starts_with("Migo-IO-")
            );
        }));
        tokio::task::spawn_blocking(move || {
            started
                .recv_timeout(std::time::Duration::from_secs(15))
                .unwrap()
        })
        .await
        .unwrap();
        cancel.store(true, Ordering::Release);
        let held = watches.iter().all(|(w, _)| w.freed_bytes().is_none());
        assert_eq!(peer.audio_cpu().run(|| 17).await.unwrap(), 17);
        release.send(()).unwrap();
        let (slot, ()) = task.await.unwrap().unwrap();
        assert!(held, "retained replay freed before original CPU returned");
        assert!(
            watches.iter().all(|(w, n)| w.freed_bytes() == Some(*n)),
            "retained buffer capacity was not actually destroyed"
        );
        drop(slot);
    }
}
