//! Same codec, PCM, SRC and public completion shape; only dispatch differs.
use super::*;
fn cpu_ns() -> u128 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut now) },
        0
    );
    now.tv_sec as u128 * 1_000_000_000 + now.tv_nsec as u128
}
fn execute(
    pool: &mut LazyDecodePool,
    results: &DecodeQueueReceiver<DecodeResult>,
    data: &Arc<Vec<u8>>,
    rate: u32,
) -> DecodedAudio {
    let (resp, _reply) = tokio::sync::oneshot::channel();
    pool.submit(DecodeJob::AudioBuffer {
        ctx_id: 1,
        target_rate: rate,
        data: data.clone(),
        resp,
    });
    let DecodeResult::AudioBuffer { result, .. } =
        results.recv_timeout(Duration::from_secs(30)).unwrap()
    else {
        panic!("buffer");
    };
    result.unwrap()
}
#[test]
#[ignore = "isolated process CPU/wall/RSS measurement"]
fn same_codec_whole_file_dispatch() {
    let case = std::env::var("MIGO_DISPATCH_CASE").unwrap();
    let loops: usize = std::env::var("MIGO_DISPATCH_LOOPS")
        .unwrap()
        .parse()
        .unwrap();
    let bytes = match case.as_str() {
        "wave" => tests::wav(48000, 384000),
        "tiny" => tests::wav(48000, 32),
        "mp3" => include_bytes!("fixtures/music.mp3").to_vec(),
        "vorbis" => include_bytes!("fixtures/cross.ogg").to_vec(),
        _ => panic!("unknown case"),
    };
    let reference = crate::decoder::decode(&bytes).unwrap();
    let data = Arc::new(bytes);
    let host = migo_io::scheduler::IoScheduler::new(0);
    let (tx, results) = decode_queue(8, MAX_DECODE_RESULT_QUEUED_BYTES);
    let mut pool = super::benchmark_pool(tx, reference.sample_rate, &host);
    let cold = Instant::now();
    assert_eq!(
        execute(&mut pool, &results, &data, reference.sample_rate).samples,
        reference.samples
    );
    let cold_ns = cold.elapsed().as_nanos();
    let mut cpu = 0;
    let mut wall = 0;
    let mut bits = 0_u64;
    for _ in 0..loops {
        let start_cpu = cpu_ns();
        let start = Instant::now();
        let actual = execute(&mut pool, &results, &data, reference.sample_rate);
        wall += start.elapsed().as_nanos();
        cpu += cpu_ns() - start_cpu;
        assert_eq!(
            (actual.channels, actual.sample_rate),
            (reference.channels, reference.sample_rate)
        );
        assert_eq!(actual.samples.len(), reference.samples.len());
        for (sample, expected) in actual.samples.iter().zip(&reference.samples) {
            assert_eq!(sample.to_bits(), expected.to_bits());
            bits = bits
                .wrapping_mul(31)
                .wrapping_add(u64::from(sample.to_bits()));
        }
    }
    println!(
        "MIGO_DISPATCH {{\"case\":\"{case}\",\"loops\":{loops},\"cpu_ns\":{cpu},\"wall_ns\":{wall},\"cold_ns\":{cold_ns},\"pcm_bits\":{bits},\"samples\":{},\"rate\":{},\"pcm_checked\":true}}",
        reference.samples.len(),
        reference.sample_rate
    );
}
