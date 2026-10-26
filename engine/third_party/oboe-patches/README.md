# Oboe 1.11.0 native input

`SOURCE.json` pins Google's release commit and archive checksum. The checked-in
projection is the unchanged `include/`, `src/`, upstream CMake source list and
license/attribution/readme files. Samples, benchmarks and repository tooling are
not compiled or vendored. There are no upstream source patches.

`migo-audio/build.rs` compiles all 62 upstream translation units plus Migo's
`native/oboe_bridge.cpp`. Android selects a single `libc++_shared` from the same
NDK as other native consumers. API choice, device workarounds, conversion and
delay-before-close remain Oboe's. The bridge uses shared stream/callback owners
and contains no copied C++ class layout. Control and destruction run on the
audio thread; the data callback only renders and runs Oboe's latency tuner.

Run `scripts/test-oboe-backend-contract.sh` for byte proof, negative controls
and the sanitized host ownership fixture. `MIGO_OBOE_UPSTREAM_ARCHIVE` can point
to a checksum-verified local copy to avoid another download. This gate proves
neither actual Android callback timing nor low latency on a device.
