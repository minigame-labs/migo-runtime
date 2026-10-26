#!/usr/bin/env python3
"""Fresh-process Release ABBA/BAAB comparison of production callback-to-refill.

The old Condvar/5ms watchdog and the new callback notification retain the same
5ms watchdog, ring, PCM and synthetic callback pacing. This measures a host
handoff, not hardware xruns, graph CPU, power or speaker latency. CPU/PSS cover
the entire child test process, including harness, callback pacing and acknowledgements.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import resource
import shutil
import statistics
import subprocess
import time


def quantile(values: list, fraction: float) -> float:
    values = sorted(values)
    return values[round((len(values) - 1) * fraction)]


def main(root: Path, output: Path, rounds: int) -> None:
    assert rounds > 0
    output.mkdir(parents=True, exist_ok=True)
    assert not any(output.iterdir()), "output directory must be empty"
    build = output / "build.jsonl"
    with build.open("w") as stream:
        result = subprocess.run(["cargo", "test", "--release", "--locked", "-p", "migo-audio",
                                 "--lib", "--no-run", "--message-format=json"], cwd=root / "engine",
                                stdout=stream, stderr=subprocess.STDOUT)
    assert result.returncode == 0, f"Release build failed: {build}"
    binaries = []
    for line in build.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get("reason") == "compiler-artifact" and row.get("executable"):
            binaries.append(Path(row["executable"]))
    assert len(binaries) == 1, binaries
    binary = binaries[0]
    sha = hashlib.sha256(binary.read_bytes()).hexdigest()
    retained = output / "benchmark-binary"
    shutil.copy2(binary, retained)
    assert hashlib.sha256(retained.read_bytes()).hexdigest() == sha
    cargo_binary = str(binary)
    binary = retained
    order = ["watchdog", "callback", "callback", "watchdog", "callback", "watchdog", "watchdog", "callback"]
    rows = []
    for index, mode in enumerate(order * rounds):
        log = output / f"{index:03}-{mode}.log"
        env = os.environ.copy()
        env["MIGO_AUDIO_WAKE_BENCH"] = mode
        cpu_before = resource.getrusage(resource.RUSAGE_CHILDREN)
        peak_pss_kib = 0
        start = time.monotonic()
        with log.open("w") as stream:
            child = subprocess.Popen([str(binary), "--exact", "output::benchmarks::audio_wake_refill_latency",
                                      "--ignored", "--nocapture"], cwd=root / "engine", env=env,
                                     stdout=stream, stderr=subprocess.STDOUT)
            while child.poll() is None:
                try:
                    memory = Path(f"/proc/{child.pid}/smaps_rollup").read_text()
                    pss = next(int(line.split()[1]) for line in memory.splitlines() if line.startswith("Pss:"))
                    peak_pss_kib = max(peak_pss_kib, pss)
                except (FileNotFoundError, ProcessLookupError):
                    pass
                time.sleep(0.01)
        cpu_after = resource.getrusage(resource.RUSAGE_CHILDREN)
        assert child.returncode == 0, f"benchmark failed: {log}"
        measurements = [json.loads(line.removeprefix("MIGO_AUDIO_WAKE_BENCH "))
                        for line in log.read_text().splitlines() if line.startswith("MIGO_AUDIO_WAKE_BENCH ")]
        assert len(measurements) == 1
        sample = measurements[0]
        assert sample["mode"] == mode and len(sample["latencies_ns"]) == sample["measured"]
        sample.update(index=index, elapsed_seconds=time.monotonic() - start, peak_pss_kib=peak_pss_kib,
                      process_cpu_seconds=(cpu_after.ru_utime + cpu_after.ru_stime - cpu_before.ru_utime - cpu_before.ru_stime))
        rows.append(sample)
        (output / "raw.json").write_text(json.dumps({"binary": str(binary), "cargo_binary": cargo_binary, "sha256": sha, "samples": rows}, indent=2) + "\n")
        print(f'{index:02} {mode}: p50={quantile(sample["latencies_ns"], .5) / 1e6:.4f}ms p95={quantile(sample["latencies_ns"], .95) / 1e6:.4f}ms', flush=True)
    summary = {"sha256": sha, "order": "ABBA+BAAB", "rounds": rounds,
               "scope": __doc__, "modes": {}}
    for mode in ("watchdog", "callback"):
        samples = [row for row in rows if row["mode"] == mode]
        values = [v for sample in samples for v in sample["latencies_ns"]]
        summary["modes"][mode] = {"processes": len(samples), "measured_callbacks": len(values),
            "latency_ms": {str(p): quantile(values, p) / 1e6 for p in (.5, .95, .99)},
            "median_process_cpu_seconds": statistics.median(row["process_cpu_seconds"] for row in samples),
            "median_peak_pss_kib": statistics.median(row["peak_pss_kib"] for row in samples)}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=4)
    args = parser.parse_args()
    main(Path(__file__).resolve().parents[2], args.output.resolve(), args.rounds)
