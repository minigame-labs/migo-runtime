//! Current production M4A index + codec reset + absolute multistage SRC.
use super::*;
use crate::stream_control::LoopRequest;
use crate::streaming::{StreamCutover, StreamPresentation};
use symphonia_core::{
    errors::{Error, SeekErrorKind},
    units::Timestamp,
};

impl<P: Packetizer> PacketStream<P> {
    pub(crate) fn reposition_window(
        &mut self,
        target: i64,
        cutover: Option<LoopRequest>,
    ) -> Result<(u64, bool), Error> {
        let reject = || Error::SeekError(SeekErrorKind::OutOfRange);
        if !self.retained || self.error.is_some() {
            return Err(Error::SeekError(SeekErrorKind::Unseekable));
        }
        let clock = self
            .clock
            .ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
        let owner = self
            .owner
            .as_mut()
            .ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
        let frames = clock.input_frames();
        let history = owner
            .history
            .ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
        // The mature SRC frees its stage Vecs at EOF. Keep only this scalar
        // clock lattice, not the completed filter allocations, for a restart.
        let period = owner.grid;
        let (mut origin, start, cycles, presentation) = if let Some(request) =
            cutover.filter(|r| r.seek_target.is_none())
        {
            let plan = StreamCutover::new(clock, request.output_start, request.enabled, history)
                .map_err(|_| reject())?;
            (
                plan.input_origin(),
                plan.output_start(),
                plan.cycles(),
                plan.presentation(),
            )
        } else {
            let frame = u64::try_from(target).map_err(|_| reject())?;
            let position = clock.position(frame).map_err(|_| reject())?;
            let start = position.frame + u64::from(position.phase != 0);
            if cutover.is_some_and(|r| r.seek_target != Some(frame) || r.output_start != start) {
                return Err(reject());
            }
            let enabled = cutover.is_some_and(|r| r.enabled);
            let presentation = if enabled {
                StreamPresentation::repeated(clock, None).map_err(|_| reject())?
            } else {
                StreamPresentation::single(clock)
            };
            (
                frame.saturating_sub(history),
                start,
                (!enabled).then_some(1),
                presentation,
            )
        };
        // Vorbis already replays its codec from the beginning. Explicit seeks
        // replay the same SRC prefix too, keeping complete suffix bits equal.
        // Dynamic loop cutovers retain their bounded absolute clock lattice.
        origin = if P::SEEK_FROM_START && cutover.is_none_or(|r| r.seek_target.is_some()) {
            0
        } else {
            origin / period * period
        };
        let end = cycles
            .map(|n| n.checked_mul(frames).ok_or_else(reject))
            .transpose()?;
        let pipeline = Stream::new_at(
            owner.decoder.rate,
            owner.target,
            owner.decoder.channels,
            end,
            origin,
            start,
        )
        .map_err(|_| reject())?;
        let terminal = presentation.output_end() == Some(start);
        if !terminal {
            let local = i64::try_from(origin % frames).map_err(|_| reject())?;
            owner.decoder.seek(Timestamp::new(local))?;
        }
        // Everything recoverably rejectable precedes these mutations. This
        // method runs only while the original AudioCpu owns the full decoder.
        owner.native.clear();
        owner.head = 0;
        owner.output.clear();
        owner.output_head = 0;
        owner.pipeline = Some(pipeline);
        owner.input_ended = false;
        owner.codec_done = false;
        owner.source_done = false;
        owner.filtered = false;
        owner.done = terminal;
        owner.window = Some(Window {
            frames,
            local: origin % frames,
            completed: origin / frames,
            cycles,
            presentation,
        });
        Ok((start, terminal))
    }
}

impl PacketStream<crate::decoder::aac::m4a::M4a> {
    pub(crate) fn reposition(
        &mut self,
        target: i64,
        cutover: Option<LoopRequest>,
    ) -> Result<(Option<crate::decoder::aac::m4a::SourceRequest>, u64, bool), Error> {
        let (start, terminal) = self.reposition_window(target, cutover)?;
        Ok((
            if terminal {
                None
            } else {
                self.source_request()
            },
            start,
            terminal,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::off_worker::OffWorker;
    use std::sync::{Arc, atomic::Ordering};
    #[tokio::test]
    async fn eof_retains_only_bounded_replay_buffers_and_original_cpu_really_frees_them() {
        let host = migo_io::scheduler::IoScheduler::new(1061);
        let peer = migo_io::scheduler::IoScheduler::new(1062);
        let slot = OffWorker::new(
            None::<PacketStream<crate::decoder::aac::m4a::M4a>>,
            host.audio_cpu(),
        );
        let (slot, watches) = slot
            .with(|slot| {
                let data = include_bytes!("../decoder/aac-fixtures/1-front.m4a");
                let mut d = PacketStream::file(16000, data.len() as u64);
                d.retain();
                let mut pos = 0;
                let mut out = Vec::with_capacity(STEP_SAMPLES);
                for _ in 0..10000 {
                    out.clear();
                    let end = (pos + 1024).min(data.len());
                    let step = d
                        .step(
                            &data[pos..end],
                            pos == data.len(),
                            &mut out,
                            &AtomicBool::new(false),
                        )
                        .unwrap();
                    pos += step.consumed;
                    assert_ne!(step.progress, Progress::NeedSource);
                    if step.progress == Progress::Finished {
                        break;
                    }
                }
                let o = d
                    .owner
                    .as_ref()
                    .expect("EOF dropped the retained source decoder");
                assert!(o.done);
                assert!(
                    o.decoder.input_len() > 0,
                    "EOF discarded the original bounded encoded window"
                );
                assert_eq!(
                    o.decoder.input_allocation().1,
                    crate::decoder::aac::incremental::ENCODED_CAPACITY
                );
                assert_eq!(
                    o.native.capacity(),
                    <crate::decoder::aac::m4a::M4a as Packetizer>::OUTPUT_SAMPLES_PER_STEP
                );
                assert_eq!(o.grid, 441, "EOF forgot the source's integer grid");
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
