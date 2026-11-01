//! Rebuild at a consumed output cursor, even after decoding future cycles.
use super::*;
use crate::{
    candidate,
    decoder_seek_contracts::{assert_pcm, resample},
    isomp4_source_contracts::fixture,
    streaming::{PcmPool, StreamingState, stream_channel},
    streaming_transport::{Decoder, Progress},
};
use std::sync::atomic::AtomicBool;

fn cycles_at(at: u64, input: u32, output: u32, frames: usize) -> usize {
    let whole = (at / u64::from(output)) * u64::from(input);
    let part = (at % u64::from(output)) * u64::from(input) / u64::from(output);
    (whole + part) as usize / frames + 1
}
fn finite<const B: bool>(data: &[u8], rate: u32, at: u64) -> usize {
    let full = candidate::decode(data, 16_777_216).unwrap();
    let frames = full.samples.len() / full.channels as usize;
    let cycles = cycles_at(at, full.sample_rate, rate, frames);
    let expected = resample(
        &full.samples.repeat(cycles),
        full.sample_rate,
        rate,
        full.channels,
    );
    let mut d = Timed::<B>::new(rate, Some(data.len() as u64));
    super::phase_tests::render(&mut d, data, 0, 137);
    d.cutover(at, false).unwrap();
    let p = d.presentation().unwrap().unwrap();
    assert_eq!(
        p.timeline().seconds(),
        frames as f64 / full.sample_rate as f64
    );
    assert_eq!(
        p.output_end(),
        Some((expected.len() / full.channels as usize) as u64)
    );
    let got = super::phase_tests::render(&mut d, data, data.len(), 137);
    assert_pcm(&got, &expected[at as usize * full.channels as usize..]);
    1
}

#[test]
fn finite_cutovers_match_whole_concatenation_at_arbitrary_output_samples() {
    let mut count = 0;
    for name in [
        "0-seek-front.m4a",
        "1-seek-front.m4a",
        "1-seek-tail.m4a",
        "1-seek-scaled.m4a",
        "1-seek-edit.m4a",
        "2-seek-front.m4a",
    ] {
        let data = fixture(name);
        let full = candidate::decode(&data, 16_777_216).unwrap();
        let n = full.samples.len() / full.channels as usize;
        for rate in [16000, 44100, 48000] {
            let end = (n as u128 * rate as u128).div_ceil(full.sample_rate as u128) as u64;
            for at in [0, 1, end + 37, 2 * end + 337] {
                count += finite::<true>(&data, rate, at);
                count += finite::<false>(&data, rate, at);
            }
        }
    }
    assert_eq!(count, 144);
    println!("W09_CUTOVER_PCM144 arbitrary output samples;whole-concatenation suffix");
}

// Only the test collector limits output. The real decoder handles source
// requests through the existing phase renderer for each finite suffix.
pub(super) fn cached_prefix<const B: bool>(
    d: &mut Timed<B>,
    frames: usize,
    channels: u32,
) -> Vec<f32> {
    let mut got = Vec::new();
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    for _ in 0..10000 {
        out.clear();
        let step = d
            .step(&[], true, &mut out, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(step.consumed, 0);
        assert!(!matches!(
            step.progress,
            Progress::Finished | Progress::Cancelled | Progress::NeedSource
        ));
        got.extend_from_slice(&out);
        if got.len() >= frames * channels as usize {
            return got;
        }
    }
    panic!("continuous cutover did not produce bounded output");
}
#[test]
fn enabling_then_disabling_uses_consumption_instead_of_decoder_lookahead() {
    let data = fixture("0-seek-front.m4a");
    let full = candidate::decode(&data, 16_777_216).unwrap();
    for rate in [16000, 44100, 48000] {
        let reference = resample(&full.samples.repeat(4096), 48000, rate, 1);
        let mut d = Timed::<true>::new(rate, Some(data.len() as u64));
        super::phase_tests::render(&mut d, &data, 0, 137);
        d.cutover(1, true).unwrap();
        let prefix = cached_prefix(&mut d, 512, 1);
        assert_pcm(&prefix, &reference[1..1 + prefix.len()]);
        let consumed = 137_u64;
        assert!(prefix.len() > consumed as usize);
        assert!(d.inner.decoded_cycles() > cycles_at(consumed, 48000, rate, 17) as u64);
        d.cutover(consumed, false).unwrap();
        let cycles = cycles_at(consumed, 48000, rate, 17);
        let expected = resample(&full.samples.repeat(cycles), 48000, rate, 1);
        assert_pcm(
            &super::phase_tests::render(&mut d, &data, data.len(), 137),
            &expected[consumed as usize..],
        );
    }
    // Shift by whole track/grid periods: the physical file is tiny but the
    // real sinc counters and cutover frame are beyond f64's exact integers.
    let shift = 2499_u64 * 4_000_000_000_000;
    let small = 2499 * 4 + 137;
    let mut d = Timed::<true>::new(44100, Some(data.len() as u64));
    super::phase_tests::render(&mut d, &data, 0, 137);
    d.cutover(shift + small, true).unwrap();
    let got = cached_prefix(&mut d, 512, 1);
    let expected = resample(&full.samples.repeat(4096), 48000, 44100, 1);
    assert_pcm(&got, &expected[small as usize..small as usize + got.len()]);
    d.cutover(shift + small, false).unwrap();
    let n = cycles_at(small, 48000, 44100, 17);
    let expected = resample(&full.samples.repeat(n), 48000, 44100, 1);
    assert_pcm(
        &super::phase_tests::render(&mut d, &data, data.len(), 137),
        &expected[small as usize..],
    );
    println!("W09_CUTOVER_LOOKAHEAD3 plus actual sinc beyond2^53");
}

#[test]
fn rejected_or_retired_cutovers_preserve_or_retire_the_exact_decoder() {
    use symphonia::core::errors::{Error, SeekErrorKind};
    let data = fixture("1-seek-front.m4a");
    let mut d = Timed::<true>::new(16000, Some(data.len() as u64));
    assert!(matches!(
        d.cutover(1, true),
        Err(Error::SeekError(SeekErrorKind::Unseekable))
    ));
    let reference = super::phase_tests::render(&mut d, &data, 0, 137);
    let before = d.presentation().unwrap();
    assert!(matches!(
        d.cutover(u64::MAX, false),
        Err(Error::SeekError(SeekErrorKind::OutOfRange))
    ));
    assert_eq!(d.presentation().unwrap(), before);
    d.seek(Timestamp::ZERO).unwrap();
    assert_pcm(
        &super::phase_tests::render(&mut d, &data, data.len(), 137),
        &reference,
    );
    d.cutover(1, true).unwrap();
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    d.step(&[], true, &mut out, &AtomicBool::new(true)).unwrap();
    assert!(d.cutover(1, false).is_err());
    let mut d = Timed::<true>::new(16000, Some(data.len() as u64));
    super::phase_tests::render(&mut d, &data, 0, 137);
    out.push(1.0);
    assert!(
        d.step(&[], true, &mut out, &AtomicBool::new(false))
            .is_err()
    );
    assert!(d.cutover(1, false).is_err());
}

#[test]
fn warmed_cached_cutovers_and_cyclic_preroll_do_not_allocate() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let data = fixture("0-seek-front.m4a");
    let mut d = Timed::<true>::new(44100, Some(data.len() as u64));
    super::phase_tests::render(&mut d, &data, 0, 137);
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    assert_no_steady_state_allocation(
        Burst {
            path: "cached output-position loop cutover",
            warmup: 64,
            measured: 256,
        },
        |i| {
            d.cutover(10001 + i as u64, i.is_multiple_of(2)).unwrap();
            for _ in 0..512 {
                out.clear();
                let step = d
                    .step(&[], true, &mut out, &AtomicBool::new(false))
                    .unwrap();
                assert!(!matches!(
                    step.progress,
                    Progress::NeedSource | Progress::Cancelled
                ));
                if !out.is_empty() {
                    break;
                }
            }
            assert!(!out.is_empty());
        },
    );
}

use crate::{
    indexed_seek::{self, SeekContext, SeekSource, SeekStatus},
    isomp4_file::IndexedFile,
    isomp4_source_contracts::{Config, client, start_server},
    isomp4_transport::IndexedHttp,
    off_worker::OffWorker,
    source_seek_contracts::{drain, take_pcm},
    streaming_transport::HttpSource,
};
use std::time::Duration;
use tokio::time::Instant;

async fn transaction<const B: bool, S: SeekSource<Timed<B>>>(
    mut source: S,
    owner: migo_io::scheduler::IoScheduler,
    data: &[u8],
    rate: u32,
) {
    let full = candidate::decode(data, 16_777_216).unwrap();
    let frames = full.samples.len() / full.channels as usize;
    let single = (frames as u128 * rate as u128).div_ceil(full.sample_rate as u128) as u64;
    let at = single + 37;
    let cycles = cycles_at(at, full.sample_rate, rate, frames);
    let expected = resample(
        &full.samples.repeat(cycles),
        full.sample_rate,
        rate,
        full.channels,
    );
    let state = StreamingState::new();
    let (tx, mut rx) = stream_channel();
    let mut pool = PcmPool::new();
    let len = data.len() as u64;
    let io_thread = std::thread::current().id();
    let empty = OffWorker::new(None::<Timed<B>>, owner.audio_cpu());
    let rejected = indexed_seek::reposition(
        &mut source,
        empty,
        |_| panic!("uninitialized mutation entered CPU"),
        pool.take(),
        SeekContext {
            tx: &tx,
            state: &state,
            deadline: Instant::now() + Duration::from_secs(3),
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        rejected.status,
        SeekStatus::Rejected(symphonia::core::errors::SeekErrorKind::Unseekable)
    ));
    drop(rejected);
    let (d, ()) = OffWorker::new(None, owner.audio_cpu())
        .with(move |slot| {
            *slot = Some(Timed::<B>::new(rate, Some(len)));
        })
        .await
        .unwrap();
    take_pcm();
    let d = drain(&mut source, d, &mut pool, &tx, &state, false).await;
    take_pcm();
    let result = indexed_seek::reposition(
        &mut source,
        d,
        move |slot| {
            assert_ne!(std::thread::current().id(), io_thread, "cutover ran on IO");
            slot.as_mut().unwrap().cutover(at, false)
        },
        pool.take(),
        SeekContext {
            tx: &tx,
            state: &state,
            deadline: Instant::now() + Duration::from_secs(3),
        },
    )
    .await
    .unwrap()
    .unwrap();
    let SeekStatus::Applied {
        pcm, step, restart, ..
    } = result.status
    else {
        panic!("valid cutover rejected")
    };
    assert_eq!(step.consumed, 0);
    let mut got = pcm.to_vec();
    drop(pcm);
    let d = if step.progress != Progress::Finished {
        drain(
            &mut source,
            result.decoder,
            &mut pool,
            &tx,
            &state,
            !restart,
        )
        .await
    } else {
        result.decoder
    };
    got.extend(take_pcm());
    assert_pcm(&got, &expected[at as usize * full.channels as usize..]);
    assert!(
        rx.try_recv().is_err(),
        "reposition published before consumer acceptance"
    );
    assert!(
        !state
            .download_complete
            .load(std::sync::atomic::Ordering::Acquire)
    );
    // Expired/canceled operations do not run the supplied codec mutation.
    let rejected = indexed_seek::reposition(
        &mut source,
        d,
        |_| panic!("expired mutation entered CPU"),
        pool.take(),
        SeekContext {
            tx: &tx,
            state: &state,
            deadline: Instant::now() - Duration::from_millis(1),
        },
    )
    .await;
    assert!(rejected.is_err());
}
#[tokio::test]
async fn real_file_http_output_cutovers_reuse_cpu_source_deadline_and_strict_pcm() {
    let mut count = 0;
    for name in ["0-seek-front.m4a", "1-seek-front.m4a", "1-seek-tail.m4a"] {
        let data = fixture(name);
        let path =
            std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
        for rate in [16000, 44100, 48000] {
            for buffered in [false, true] {
                let owner = migo_io::scheduler::IoScheduler::new(0);
                let source = IndexedFile::open(std::fs::File::open(&path).unwrap(), &owner)
                    .await
                    .unwrap();
                if buffered {
                    transaction::<true, _>(source, owner, &data, rate).await;
                } else {
                    transaction::<false, _>(source, owner, &data, rate).await;
                }
                let server = start_server(data.clone(), Config::default()).await;
                let client = client();
                let response = <IndexedHttp as HttpSource<Timed<false>>>::request(client.get(
                    crate::network::gate::GateKind::AudioStream,
                    server.url.clone(),
                ))
                .send()
                .await
                .unwrap();
                let source =
                    <IndexedHttp as HttpSource<Timed<false>>>::new(client, response).unwrap();
                let owner = migo_io::scheduler::IoScheduler::new(0);
                if buffered {
                    transaction::<true, _>(source, owner, &data, rate).await;
                } else {
                    transaction::<false, _>(source, owner, &data, rate).await;
                }
                assert_eq!(
                    server
                        .full_requests
                        .load(std::sync::atomic::Ordering::Acquire),
                    1
                );
                count += 2;
            }
        }
    }
    assert_eq!(count, 36);
    println!("W09_CUTOVER_SOURCE36 exact File/HTTP CPU transactions");
}
