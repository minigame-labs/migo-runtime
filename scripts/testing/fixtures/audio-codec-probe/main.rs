// The runner prepends paths to the actual legacy decoder/limits and candidate
// module. Both arms use the same DecodedAudio type and encoded input fixture.
use std::{io::Write, time::Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("legacy or candidate");
    let input = args.next().expect("audio input");
    let cap = args
        .next()
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or(limits::MAX_AUDIO_PCM_SAMPLES as usize);
    let dump = args.next();
    if std::fs::metadata(&input).unwrap().len() > 16 * 1024 * 1024 {
        println!(
            "{}",
            serde_json::json!({"ok": false, "mode": mode,
            "elapsed_ns": 0, "error": "encoded audio exceeds 16MiB"})
        );
        std::process::exit(2);
    }
    let data = std::fs::read(&input).unwrap();
    let started = Instant::now();
    let result = match mode.as_str() {
        "legacy" => decoder::decode(&data).map_err(|error| format!("{error:?}")),
        "candidate" => candidate::decode(&data, cap),
        "candidate-unseekable" => candidate::decode_source(&data, cap, false),
        _ => panic!("unknown codec arm"),
    };
    let elapsed_ns = started.elapsed().as_nanos();
    match result {
        Ok(audio) => {
            let finite = audio.samples.iter().all(|sample| sample.is_finite());
            let energy: f64 = audio.samples.iter().map(|v| f64::from(*v).powi(2)).sum();
            let peak = audio
                .samples
                .iter()
                .fold(0.0f32, |peak, value| peak.max(value.abs()));
            if let Some(path) = dump {
                let mut stream = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
                for sample in &audio.samples {
                    stream.write_all(&sample.to_le_bytes()).unwrap();
                }
                stream.flush().unwrap();
            }
            println!(
                "{}",
                serde_json::json!({
                    "ok": true, "mode": mode, "elapsed_ns": elapsed_ns,
                    "sample_rate": audio.sample_rate, "channels": audio.channels,
                    "frames": audio.frame_count(), "samples": audio.samples.len(),
                "pcm_capacity_samples": audio.samples.capacity(),
                    "duration_seconds": audio.duration(), "finite": finite,
                    "energy": energy, "peak": peak,
                })
            );
        }
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({"ok": false, "mode": mode,
                "elapsed_ns": elapsed_ns, "error": error})
            );
            std::process::exit(2);
        }
    }
}
