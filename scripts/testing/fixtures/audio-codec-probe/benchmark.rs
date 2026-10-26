// Full-buffer decode microbenchmark only. Output checks are performed by the
// separate conformance runner; do not time PCM serialization or energy scans.
use std::{hint::black_box, time::Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap();
    let input = args.next().unwrap();
    let iterations: usize = args.next().unwrap().parse().unwrap();
    assert!((1..=1024).contains(&iterations));
    assert!(std::fs::metadata(&input).unwrap().len() <= 16 * 1024 * 1024);
    let data = std::fs::read(input).unwrap();
    let mut durations = Vec::with_capacity(iterations);
    let mut shape = None;
    for _ in 0..iterations {
        let start = Instant::now();
        let audio = match mode.as_str() {
            "legacy" => decoder::decode(black_box(&data)).map_err(|e| format!("{e:?}")),
            "candidate" => candidate::decode(black_box(&data), 16_777_216),
            _ => panic!("unknown benchmark arm"),
        }
        .unwrap();
        let current = (audio.sample_rate, audio.channels, audio.frame_count());
        assert_eq!(*shape.get_or_insert(current), current);
        black_box(&audio.samples);
        // Include freeing the output in the operation: no retained prior PCM
        // may inflate the next iteration's high-water memory measurement.
        drop(audio);
        durations.push(start.elapsed().as_nanos() as u64);
    }
    let cold_ns = durations[0];
    let total_ns: u64 = durations.iter().sum();
    durations.sort_unstable();
    let (rate, channels, frames) = shape.unwrap();
    println!(
        "{}",
        serde_json::json!({
            "mode": mode, "iterations": iterations, "cold_decode_and_drop_ns": cold_ns,
            "decode_and_drop_total_ns": total_ns, "median_operation_ns": durations[iterations/2],
            "sample_rate": rate, "channels": channels, "frames_per_iteration": frames,
            "encoded_bytes": data.len(),
        })
    );
}
