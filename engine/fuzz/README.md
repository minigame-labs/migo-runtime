# Fuzz targets

Not workspace members: cargo-fuzz builds with its own profile and sanitizer
flags, and making these members would push those flags onto every other crate.
Each target declares an empty `[workspace]` so cargo treats it as its own root.

```sh
cargo +nightly fuzz run envelope --fuzz-dir engine/fuzz/frame-wire
cargo +nightly fuzz run render_stream --fuzz-dir engine/fuzz/frame-decode
```

Seed the corpus from `contracts/frame-wire/golden/`. Those are the shapes a real
producer emits, so a fuzzer that starts there spends its budget on mutations of
valid packets rather than rediscovering the magic number.

| Target | Property |
|---|---|
| `frame-wire/envelope` | no input makes the cross-process frame parser panic, read out of bounds, loop unboundedly, or allocate a size the input chose |
| `frame-decode/render_stream` | no structurally valid command stream makes the decode pass panic, read out of bounds, loop unboundedly, or allocate beyond the frame budget the admission check enforces; records the validators refuse are skipped, not faults |

Run history (record a run here when a target changes or a long run is done; a clean run is evidence, a crash is a bug with the artifact to reproduce it):

| Date | Target | Result |
|---|---|---|
| 2026-10-01 | `frame-decode/render_stream` | 22.8 M executions in 10 minutes from an empty corpus (nightly 2026-09-30, macOS x86_64, ASan), coverage 1478 edges, no crash. Resident memory climbs to roughly 800 MB over the run; with only the structural pass in the target it climbs the same way, so it is the sanitizer's quarantine and the fuzzer's bookkeeping, not the decoder. |
