# Bounded minimp3 presentation candidate

`UPSTREAM` pins the published minimp3-sys 0.6.0 crate by SHA-256. The acceptance
script extracts that archive and applies this patch with zero fuzz. The product workspace selects the materialized source under
`engine/third_party/minimp3-sys-0.6.0`; the product cursor/job integration is
checked by `scripts/testing/check_audio_product_mp3.py`.

The private C interface keeps the existing minimp3 decoder and free-format
support. `begin` consumes a first frame already measured by that decoder and
calls `mp3dec_load_index` once with `MP3D_DO_NOT_SCAN`. It neither opens an IO
reader nor builds a seek index. The native unit disables stdio entry points. A metadata quota is checked before any PCM
allocation; counts include every channel. `decode` accepts at most 8192 bytes
and calls the original frame decoder once. Frame delay/reservoir handling and
presentation-end clamping use helpers factored directly from
`mp3dec_ex_read_frame`; the upstream reader calls those same helpers. No new
Xing/LAME parser or Rust trimming formula is introduced. Bounds checks precede
the existing parser's optional-field reads, and frame-count shifts use unsigned
operands. `finish` rejects incomplete declared presentations.

The caller still owns frame measurement, lookahead, cancellation, quotas and
publication. The first free-format decode needs its established native
lookahead view; later frames can use the cursor's measured exact view, including
padded frames before an ID3v1 tag. The standalone test uses fixture-known frame
boundaries for that case. It does not validate the product cursor integration.

`MIGO_MINIMP3_TEST` only enables allocation counters, compiler entry hooks and
a correctness oracle through the original full reader. Run tests with one
thread. Entry hooks count actual `mp3dec_decode_frame` calls in debug/release;
the oracle is unbounded and never counted as a scheduling step.

Run `python3 scripts/testing/check_audio_minimp3_presentation.py --output /tmp/NAME
--target-dir /tmp/TARGET`. `--sanitize` uses native ASan/UBSan and a separate target
folder. `--plain` verifies the release ABI/PCM without test hooks. No mode changes the product manifest/lock or creates a new runtime
backend. The native gate alone does not validate the product cursor or scheduler.
Use the separate product gate for cursor/job cancellation and complete PCM;
platform/device and equal-PCM performance remain separate acceptance.

The upstream MIT/CC0 notices stay in the extracted source. This additive patch
follows the repository's licensing; it does not replace those notices.

The third patch adds a progressive metadata entry point over the same internal
initializer. The existing whole-file entry point retains the native 64 MiB PCM
ceiling, including callers requesting a larger limit. The progressive entry
point retains no full PCM allocation and uses the existing bounded input/output
windows; its declared presentation is not restricted by that whole-file ceiling.
The Rust progressive driver separates native owner allocation and metadata into
individual CPU steps and validates the declared presentation before success.

The native build script explicitly watches the C translation unit, private
adapter header and both upstream headers. Cargo must rebuild after a source or
header edit even when cc emits environment watch rules. A cached native artifact
is never accepted as an executed source fault.
