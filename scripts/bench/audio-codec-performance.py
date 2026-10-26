#!/usr/bin/env python3
"""W09 fresh-process Release ABBA/BAAB full-buffer codec comparison.

Use identical synthetic encoded inputs and one binary per SIMD selection for
both arms. Time actual decode plus output destruction, and retain process CPU
and peak RSS separately. This does not measure progressive first-sample latency,
hardware playback, DSP, mobile power or whole-runtime memory.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import random
import statistics
import subprocess

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("audio_codecs", ROOT / "scripts/bench/audio-codecs.py")
codecs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(codecs)


def interval(values: list[float]) -> list[float]:
    rng = random.Random(901)
    samples = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(4096))
    return [samples[int(len(samples) * q)] for q in (0.025, 0.975)]


def main(output: Path, target: Path) -> None:
    assert not output.exists(), "use a fresh performance directory"
    assert os.name == "posix" and Path("/usr/bin/time").exists()
    output.mkdir(parents=True)
    original_lock = (ROOT / "engine/Cargo.lock").read_bytes()
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN="1.95.0", CARGO_TARGET_DIR=str(target))
    allowed = sorted(os.sched_getaffinity(0))
    cpu = allowed[0]
    provenance = {"allowed_cpus": allowed, "child_affinity_cpu": cpu,
                  "cpuinfo": Path("/proc/cpuinfo").read_text(),
                  "kernel": os.uname().release, "scope": __doc__}
    (output / "environment.json").write_text(json.dumps(provenance, indent=2) + "\n")
    codecs.run(["rustc", "-Vv"], output / "rustc.txt", cwd=ROOT, env=env)
    codecs.run(["ffmpeg", "-version"], output / "ffmpeg.txt", cwd=ROOT)
    binaries = {}
    for simd in (False, True):
        work = output / ("simd" if simd else "scalar")
        work.mkdir()
        binary = codecs.make_package(ROOT, work, env, simd, True, True)
        binaries[simd] = binary
    fixtures = []
    for rate, channels, frames, repeats, label in [(44100, 1, 8957, 64, "short-mono"),
                                                  (48000, 2, 480137, 4, "music-stereo")]:
        directory = output / label
        directory.mkdir()
        pcm = directory / "source.wav"
        codecs.fixture_pcm(pcm, rate, channels, frames)
        for name, suffix, args in [("wav16", "wav", []),
                                    ("mp3", "mp3", ["-c:a", "libmp3lame", "-b:a", "128k"]),
                                    ("vorbis", "ogg", ["-c:a", "libvorbis", "-q:a", "4"])]:
            source = pcm
            if args:
                source = directory / f"encoded.{suffix}"
                codecs.run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(pcm), *args, str(source)],
                           directory / f"encode-{name}.log", cwd=output)
            fixtures.append({"name": f"{label}-{name}", "source": source,
                             "frames": frames, "repeats": repeats, "rate": rate, "channels": channels})

    def setup() -> None:
        codecs.limits()
        os.sched_setaffinity(0, {cpu})

    rows = []
    # Eight blocks, alternating ABBA and BAAB. Rotate case order between blocks
    # to avoid assigning all late samples to one codec or SIMD selection.
    groups = [(simd, fixture) for simd in (False, True) for fixture in fixtures]
    for block in range(8):
        ordered = groups[block % len(groups):] + groups[:block % len(groups)]
        arms = ["legacy", "candidate", "candidate", "legacy"] if block % 2 == 0 \
            else ["candidate", "legacy", "legacy", "candidate"]
        for simd, fixture in ordered:
            for position, arm in enumerate(arms):
                label = f'{"simd" if simd else "scalar"}-{fixture["name"]}-{block}-{position}-{arm}'
                measured = output / f"{label}-process.json"
                command = ["/usr/bin/time", "-f", '{"user_s":%U,"system_s":%S,"peak_rss_kib":%M,"wall_s":%e}',
                           "-o", str(measured), str(binaries[simd]), arm,
                           str(fixture["source"]), str(fixture["repeats"])]
                child = subprocess.run(command, env=env, capture_output=True, text=True, timeout=60,
                                       preexec_fn=setup)
                (output / f"{label}.log").write_text(child.stdout + child.stderr)
                assert child.returncode == 0, (label, child.stderr)
                result = json.loads(child.stdout)
                assert result["sample_rate"] == fixture["rate"] and result["channels"] == fixture["channels"]
                if arm == "candidate":
                    assert result["frames_per_iteration"] == fixture["frames"], result
                row = {"simd": simd, "case": fixture["name"], "block": block,
                       "position": position, "arm": arm, "fixture_sha256": codecs.sha(fixture["source"]),
                       "binary_sha256": codecs.sha(binaries[simd]), **result,
                       "process": json.loads(measured.read_text())}
                rows.append(row)
                (output / "raw.json").write_text(json.dumps(rows, indent=2) + "\n")
        print(f"codec ABBA/BAAB block {block + 1}/8 complete", flush=True)
    summaries = []
    for simd, fixture in groups:
        group = [row for row in rows if row["simd"] == simd and row["case"] == fixture["name"]]
        ratios = []
        cold_ratios = []
        warm_ratios = []
        for block in range(8):
            medians = {arm: statistics.median(row["decode_and_drop_total_ns"] / row["iterations"]
                       for row in group if row["block"] == block and row["arm"] == arm)
                       for arm in ("legacy", "candidate")}
            ratios.append(medians["candidate"] / medians["legacy"])
            for metric, series in ((lambda row: row["cold_decode_and_drop_ns"], cold_ratios),
                                   (lambda row: (row["decode_and_drop_total_ns"] - row["cold_decode_and_drop_ns"])
                                    / (row["iterations"] - 1), warm_ratios)):
                values = {arm: statistics.median(metric(row) for row in group
                          if row["block"] == block and row["arm"] == arm)
                          for arm in ("legacy", "candidate")}
                series.append(values["candidate"] / values["legacy"])
        arms = {}
        for arm in ("legacy", "candidate"):
            samples = [row for row in group if row["arm"] == arm]
            arms[arm] = {"samples": len(samples),
                "decode_and_drop_ns_median": statistics.median(row["decode_and_drop_total_ns"] / row["iterations"] for row in samples),
                "cold_decode_and_drop_ns_median": statistics.median(row["cold_decode_and_drop_ns"] for row in samples),
                "warm_decode_and_drop_ns_median": statistics.median(
                    (row["decode_and_drop_total_ns"] - row["cold_decode_and_drop_ns"]) / (row["iterations"] - 1)
                    for row in samples),
                "process_cpu_s_median": statistics.median(row["process"]["user_s"] + row["process"]["system_s"] for row in samples),
                "process_peak_rss_kib_median": statistics.median(row["process"]["peak_rss_kib"] for row in samples),
                "frames_per_iteration": sorted({row["frames_per_iteration"] for row in samples})}
        summaries.append({"simd": simd, "case": fixture["name"], "arms": arms,
                          "paired_decode_ratio_median": statistics.median(ratios),
                          "paired_block_ratio_bootstrap_95pct": interval(ratios),
                          "paired_cold_ratio_median": statistics.median(cold_ratios),
                          "paired_cold_ratio_bootstrap_95pct": interval(cold_ratios),
                          "paired_warm_ratio_median": statistics.median(warm_ratios),
                          "paired_warm_ratio_bootstrap_95pct": interval(warm_ratios)})
    assert (ROOT / "engine/Cargo.lock").read_bytes() == original_lock
    summary = {"fresh_processes": len(rows), "blocks_per_case": 8,
               "root_lock_unchanged": True, "production_adoption_ready": False,
               "results": summaries, "scope": __doc__}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve())
