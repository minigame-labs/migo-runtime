#!/usr/bin/env python3
"""W09 diagnostic startup phases: separate wall time from thread CPU time.

Instrumentation has overhead and is not a substitute for the uninstrumented
ABBA comparison. Registries, format probing, decoder construction and the decode
loop are timed separately; no initialization is moved out of the measured work.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import statistics
import subprocess

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("audio_codecs", ROOT / "scripts/bench/audio-codecs.py")
codecs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(codecs)


def main(output: Path, target: Path, fixtures: Path) -> None:
    assert not output.exists(), "use a fresh diagnostic directory"
    assert os.uname().sysname == "Linux", "thread CPU diagnostic currently requires Linux"
    output.mkdir(parents=True)
    original_lock = (ROOT / "engine/Cargo.lock").read_bytes()
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN="1.95.0", CARGO_TARGET_DIR=str(target))
    cpu = min(os.sched_getaffinity(0))

    def setup() -> None:
        codecs.limits()
        os.sched_setaffinity(0, {cpu})

    names = ["wav16-44100-1/source.wav", "mp3-cbr-44100-1/encoded.mp3",
             "vorbis-44100-1/encoded.ogg", "aac-m4a-48000-2/encoded.m4a"]
    rows = []
    for simd in (False, True):
        work = output / ("simd" if simd else "scalar")
        work.mkdir()
        binary = codecs.make_package(ROOT, work, env, simd, True, False, True)
        for name in names:
            source = fixtures / name
            for repeat in range(3):
                result = subprocess.run([str(binary), "candidate", str(source), "16777216"],
                                        text=True, capture_output=True, preexec_fn=setup, timeout=30)
                (work / f'{Path(name).parent.name}-{repeat}.log').write_text(result.stdout + result.stderr)
                assert result.returncode == 0, result.stderr
                markers = [line.removeprefix("W09_PHASES ") for line in result.stderr.splitlines()
                           if line.startswith("W09_PHASES ")]
                assert len(markers) == 1, result.stderr
                phases = {stage: {"wall_ns": wall, "thread_cpu_ns": thread_cpu}
                          for stage, wall, thread_cpu in json.loads(markers[0])}
                assert len(phases) == 8 and all(p["thread_cpu_ns"] is not None for p in phases.values())
                rows.append({"simd": simd, "fixture": name, "repeat": repeat, "phases": phases,
                             "binary_sha256": codecs.sha(binary), "fixture_sha256": codecs.sha(source),
                             "result": json.loads(result.stdout)})
                (output / "raw.json").write_text(json.dumps(rows, indent=2) + "\n")
    summaries = []
    for simd in (False, True):
        for name in names:
            selected = [row for row in rows if row["simd"] == simd and row["fixture"] == name]
            phases = {stage: {metric: statistics.median(row["phases"][stage][metric] for row in selected)
                              for metric in ("wall_ns", "thread_cpu_ns")}
                      for stage in selected[0]["phases"]}
            summaries.append({"simd": simd, "fixture": name, "phases_median": phases,
                              "wall_total_median_ns": statistics.median(sum(p["wall_ns"] for p in row["phases"].values()) for row in selected),
                              "thread_cpu_total_median_ns": statistics.median(sum(p["thread_cpu_ns"] for p in row["phases"].values()) for row in selected)})
    assert (ROOT / "engine/Cargo.lock").read_bytes() == original_lock
    summary = {"diagnostic_processes": len(rows), "child_cpu": cpu, "results": summaries,
               "production_lock_unchanged": True, "production_adoption_ready": False,
               "scope": __doc__}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--fixtures-dir", type=Path, required=True)
    args = parser.parse_args()
    main(args.output.resolve(), args.target_dir.resolve(), args.fixtures_dir.resolve())
