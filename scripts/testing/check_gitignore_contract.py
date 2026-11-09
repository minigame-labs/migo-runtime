"""Source and generated-output boundaries for current and earlier work."""
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/lib'))
from gitignore_policy import check, tracked_ignored

REQUIRED = [
    'engine/crates/audio/src/inner_rate_tests.rs',
    'engine/crates/audio/src/variable_rate.rs',
    'engine/crates/audio/native/variable_rate.c',
    'engine/third_party/libsamplerate-source.json',
    'engine/third_party/libsamplerate-0.2.2/COPYING',
    'engine/third_party/libsamplerate-0.2.2/AUTHORS',
    'engine/third_party/libsamplerate-0.2.2/include/samplerate.h',
    'engine/third_party/libsamplerate-0.2.2/src/common.h',
    'engine/third_party/libsamplerate-0.2.2/src/samplerate.c',
    'engine/third_party/libsamplerate-0.2.2/src/src_sinc.c',
    'engine/third_party/libsamplerate-0.2.2/src/src_linear.c',
    'engine/third_party/libsamplerate-0.2.2/src/src_zoh.c',
    'engine/third_party/libsamplerate-0.2.2/src/fastest_coeffs.h',
    'scripts/testing/fixtures/libsamplerate-0.2.2-retain-history.patch',
    'engine/third_party/symphonia-format-ogg-patches/0004-fragmented-first-page-restore.patch',
    'engine/crates/audio/src/streaming_http_vorbis_tests.rs',
    'engine/crates/audio/src/streaming_file_vorbis_tests.rs',
    'engine/third_party/symphonia-format-ogg-patches/0003-pinned-vorbis-presentation-replay.patch',
    'engine/crates/audio/src/streaming_http_mp3_tests.rs',
    'engine/crates/audio/src/streaming_file_mp3_tests.rs',
    'engine/crates/audio/src/streaming_http_wave_tests.rs',
    'engine/crates/audio/src/streaming_file_control_tests.rs',
    'engine/crates/audio/src/streaming_sinc_origin_tests.rs',
    'engine/crates/io/src/source_snapshot/package.rs',
    'engine/crates/io/src/source_snapshot/package/tests.rs',
    'engine/crates/services/src/audio_package_file_tests.rs',
    'engine/crates/audio/src/streaming_file_index_tests.rs',
    'engine/crates/shared/src/audio_file.rs',
    'engine/crates/services/src/audio_file_stream.rs',
    'engine/crates/services/src/audio_file_stream_tests.rs',
    'engine/crates/audio/src/streaming_file_tests.rs',
    'engine/testing/alloc-probe/src/deallocation_watch.rs',
    'scripts/testing/fixtures/oboe/bridge_test.cpp',
    'scripts/testing/fixtures/oboe/oboe/Oboe.h',
    'scripts/test-oboe-backend-contract.sh',
    'engine/crates/audio/src/output_ring.rs',
    'engine/crates/audio/src/audio_thread_mix_buffer.rs',
    'engine/crates/audio/src/audio_thread_context_table.rs',
    'engine/crates/audio/src/audio_thread_inner_players.rs',
    'engine/crates/audio/src/audio_thread_media_players.rs',
    'engine/crates/audio/src/audio_thread_node_index.rs',
    'engine/crates/audio/src/audio_thread_node_creation.rs',

    'engine/crates/audio/src/nodes/wave_shaper_inline_tests.rs',
    'engine/crates/audio/src/nodes/wave_shaper_inline_reference.rs',
    'engine/crates/audio/src/inline_automation_tests.rs',
    'engine/crates/audio/src/nodes/inline_automation_gain_reference.rs',
    'engine/crates/audio/src/nodes/inline_automation_delay_reference.rs',
    'engine/crates/audio/src/nodes/inline_automation_constant_source_reference.rs',
    'engine/crates/audio/src/nodes/inline_automation_oscillator_reference.rs',
    'engine/crates/audio/src/nodes/inline_automation_param_reference.rs',
    'engine/crates/audio/src/audio_thread_filter_storage_tests.rs',
    'engine/crates/audio/src/nodes/filter_storage_biquad_filter_reference.rs',
    'engine/crates/audio/src/nodes/filter_storage_iir_filter_reference.rs',
    'engine/crates/audio/src/nodes/analyser_workspace.rs',
    'engine/crates/audio/src/audio_thread_analyser_output.rs',
    "engine/crates/audio/src/stream_pcm_buffer.rs",
    "engine/crates/audio/src/audio_thread_command_frequency.rs",
    "engine/crates/audio/src/audio_thread_command_parameters.rs",
    "engine/crates/audio/src/audio_thread_command_copy.rs",
    "engine/crates/core/src/services/audio.rs",

    "engine/crates/audio/src/context_source_completion_tests.rs",
    "engine/crates/audio/src/context_feedback_tests.rs",
    "engine/crates/audio/src/context_processing.rs",
    "engine/crates/audio/src/context_source_scheduling_tests.rs",
    "engine/crates/audio/src/render_time.rs",
    "engine/crates/audio/src/context_source_automation_tests.rs",
    "engine/crates/audio/src/context_waveshaper_tests.rs",
    "engine/crates/io/src/audio_preparation_tests.rs",
    "engine/crates/audio/src/context_capacity_tests.rs",
    "engine/crates/audio/src/device_mixer.rs",
    "engine/crates/audio/src/device_mixer_tests.rs",
    "engine/crates/audio/src/device_graph.rs",
    "engine/crates/audio/src/device_graph_tests.rs",
    "engine/crates/audio/src/context_route_tests.rs",
    "engine/crates/audio/src/device_owner_slot.rs",
    "engine/crates/audio/src/device_owner_slot_tests.rs",
    "engine/crates/audio/src/device_resources.rs",
    "engine/crates/audio/src/device_resources_tests.rs",
    "engine/crates/audio/src/device_session.rs",
    "engine/crates/audio/src/device_session_tests.rs",
    "engine/crates/audio/src/streaming_sinc.rs",
    "engine/crates/audio/src/streaming_sinc_tests.rs",
    "engine/crates/audio/src/context_device_tests.rs",
    "engine/crates/audio/src/context_clock_tests.rs",
    "engine/crates/audio/src/device_prepare.rs",
    "engine/crates/audio/src/device_prepare_tests.rs",
    "engine/crates/audio/src/device_pull.rs",
    "engine/crates/audio/src/device_pull_tests.rs",
    "engine/crates/audio/src/decode_pool.rs",
    "engine/crates/audio/src/decode_pool_tests.rs",
    'engine/crates/audio/src/decoder/whole.rs',
    'engine/crates/audio/src/decoder/whole_tests.rs',
    "engine/third_party/rubato-source-patches/0001-keep-published-lockfiles.patch",
    "engine/third_party/rubato-source-patches/README.md",
    "engine/crates/audio/src/bandlimited.rs",
    "engine/third_party/rubato-sources.json",
    "engine/third_party/symphonia-format-riff-patches/0003-preserve-pcm-storage-and-discrete-channels.patch",
    'engine/crates/audio/src/decoder/mp3/fixtures/gapless-mpeg2.i16le',
    'engine/crates/audio/src/decoder/mp3/fixtures/gapless-mpeg2.mp3',
    'engine/crates/audio/src/decoder/mp3/fixtures/gapless-source.json',
    'engine/crates/audio/src/decoder/mp3/fixtures/tiny-47-mono.i16le',
    'engine/crates/audio/src/decoder/mp3/fixtures/tiny-47-mono.mp3',
    'engine/crates/audio/src/decoder/mp3/fixtures/tiny-47-stereo.i16le',
    'engine/crates/audio/src/decoder/mp3/fixtures/tiny-47-stereo.mp3',
    'engine/crates/audio/src/decoder/mp3/fixtures/music.i16le',
    'engine/crates/audio/src/decoder/mp3/fixtures/music.mp3',
    'platforms/apple/WebContent/PerformancePlus/test/fixtures/audio-check-answers.json',
    'engine/crates/io/src/audio_cpu_background_tests.rs',
    'engine/crates/audio/src/decoder/pcm.rs',
    'engine/crates/audio/src/decoder/pcm_tests.rs',
    'scripts/testing/generate_mp3_step_fixtures.py',
    'engine/crates/audio/src/decoder/mp3/fixtures/stereo-cbr.mp3',
    'engine/crates/audio/src/decoder/mp3/fixtures/stereo-cbr.f32le',
    'engine/crates/audio/src/decoder/mp3/fixtures/mono-vbr.mp3',
    'engine/crates/audio/src/decoder/mp3/fixtures/mono-vbr.f32le',
    'engine/crates/audio/src/decoder/mp3/fixtures/source.json',
    'engine/crates/audio/src/decoder/mp3/fixtures/README.md',
    'engine/crates/audio/src/resample_job.rs',
    'engine/crates/audio/src/resample_job_tests.rs',
    'scripts/testing/check_service_op_retirement.py',
    'engine/crates/audio/src/loading_control_tests.rs',
    'engine/crates/audio/src/cyclic_seek_tests.rs',
    'engine/crates/audio/src/live_loop_tests.rs',
    'engine/crates/audio/src/stream_cutover_tests.rs',
    'engine/crates/audio/src/cyclic_clock_tests.rs',
    'engine/crates/audio/src/stream_control.rs',
    'engine/crates/audio/src/public_control_tests.rs',
    'engine/crates/audio/src/stream_timeline_tests.rs',
    'engine/crates/audio/src/off_worker.rs',
    'engine/crates/services/src/network/http/range/interruption_tests.rs',
    'engine/crates/audio/src/stream_revision_tests.rs',
    'engine/third_party/symphonia-format-isomp4-patches/0009-streaming-seek-source-handoff.patch',
    'engine/third_party/symphonia-format-isomp4-patches/0008-shared-presentation-seek.patch',
    'engine/crates/audio/src/forward_seek_tests.rs',
    'engine/crates/audio/src/seek_tests.rs',
    'engine/crates/shared/src/audio_load.rs',
    'platforms/apple/WebContent/PerformancePlus/test/source-load.test.mjs',
    'engine/crates/audio/src/source_load_tests.rs',
    'engine/crates/audio/src/inner_audio.rs',
    'engine/third_party/symphonia-core-patches/0007-defer-probed-container-construction.patch',
    'engine/Cargo.lock', 'tools/artifact-manifest/Cargo.lock',
    'engine/third_party/rustls-platform-verifier-android-0.2.0/classes.jar',
    'engine/third_party/rustls-platform-verifier-android-0.2.0/rustls-platform-verifier-0.2.0.aar',
    'engine/crates/runtime-v8/snapshots/SNAPSHOT-full-linux-x86_64.bin',
    'contracts/android-runtime-dependencies.json',
    'developer-docs/src/content/docs/reference/session.mdx',
    'developer-docs/public/brand/mark.svg',
    'engine/crates/io/src/polling.rs', 'scripts/lib/gitignore_policy.py',
    'engine/crates/core/src/lib.rs',
    'engine/crates/audio/native/oboe_bridge.cpp', 'engine/crates/audio/native/oboe_bridge.h',
    'engine/third_party/oboe-1.11.0/LICENSE', 'engine/third_party/oboe-patches/SOURCE.json',
    'engine/crates/io/src/audio_cpu.rs',
    'scripts/testing/check_audio_cpu_admission.py',
    'engine/crates/io/src/spool.rs',
    'engine/crates/io/src/spool/tests.rs',
    'engine/crates/io/src/fs_ops/mutation_tests.rs',
    'engine/crates/shared/src/vfs/file_mutation.rs',
    'engine/crates/shared/src/vfs/file_mutation/tests.rs',
    'engine/crates/io/src/package_ingest/mutation_tests.rs',
    'engine/crates/services/src/storage/buffer_url_tests.rs',
    'engine/crates/shared/src/services/host_files/mutation_tests.rs',
    'engine/crates/shared/src/vfs/write_directory.rs',
    'engine/crates/shared/src/vfs/write_directory/tests.rs',
    'engine/crates/services/src/storage/buffer_url_namespace_tests.rs',
    'engine/crates/shared/src/vfs/game_paths/storage_tests.rs',
    'engine/crates/io/src/storage_migration.rs',
    'engine/crates/io/src/storage_migration/tests.rs',
    'engine/crates/services/src/storage/private_tests.rs',
    'engine/crates/io/src/source_snapshot.rs',
    'engine/crates/io/src/source_snapshot/tests.rs',
    'engine/crates/shared/src/vfs/mount/asset_tests.rs',
    'engine/crates/services/src/fs_copy_budget_tests.rs',
    'engine/crates/shared/src/vfs/file_copy.rs',
    'engine/crates/shared/src/vfs/file_copy/tests.rs',
    'engine/crates/shared/src/vfs/mount/copy_tests.rs',
    'engine/crates/shared/src/vfs/package/entry_reader/copy_tests.rs',
    'engine/crates/io/src/source_copy.rs',
    'engine/crates/io/src/source_copy/tests.rs',
    'engine/crates/io/src/zip_extract/mutation_tests.rs',
    'engine/crates/io/src/read_at_file.rs',
    'engine/crates/io/src/read_at_package.rs',
    'engine/crates/io/src/read_at_package/tests.rs',
    'engine/crates/shared/src/vfs/read_root.rs',
    'engine/crates/shared/src/vfs/read_root/tests.rs',
    'engine/crates/shared/src/vfs/file_io.rs',
    'engine/crates/shared/src/vfs/package/entry_reader.rs',
    'engine/crates/shared/src/vfs/package/entry_reader/tests.rs',
    'engine/third_party/symphonia-format-isomp4-patches/0006-bind-pinned-stream-length.patch',
    'engine/third_party/symphonia-format-isomp4-patches/0007-resolve-buffered-streaming-sources.patch',
    'engine/crates/services/src/network/http/range.rs',
    'engine/crates/services/src/network/http/range/stream.rs',
    'engine/crates/services/src/network/http/range/tests.rs',
    'engine/third_party/symphonia-core-patches/0006-borrowed-slice-buffered-seek.patch',
    'engine/third_party/symphonia-format-isomp4-patches/0004-borrowed-aac-streaming-index.patch',
    'engine/third_party/symphonia-format-isomp4-patches/0005-borrowed-sequential-aac-packets.patch',
    'engine/third_party/symphonia-format-ogg-patches/0002-borrowed-incremental-vorbis-framing.patch',
    'engine/third_party/symphonia-core-patches/0005-lightweight-bitstream-eof.patch',
    'engine/third_party/symphonia-codec-vorbis-patches/UPSTREAM',
    'engine/third_party/symphonia-codec-vorbis-patches/README.md',
    'engine/third_party/symphonia-codec-vorbis-patches/0001-handle-lightweight-bitstream-eof.patch',
    'engine/third_party/symphonia-codec-aac-patches/0002-borrowed-incremental-adts-framing.patch',
    'engine/third_party/symphonia-format-riff-patches/0002-borrowed-incremental-wave-framing.patch',
    'engine/third_party/symphonia-codec-pcm-patches/UPSTREAM',
    'engine/third_party/symphonia-codec-pcm-patches/README.md',
    'engine/third_party/symphonia-codec-pcm-patches/0001-decode-exact-pcm-packet-frames.patch',
    'engine/third_party/symphonia-core-patches/0004-keep-profile-plane-views-on-stack.patch',
    'engine/third_party/symphonia-core-patches/0002-enforce-id3-metadata-budgets.patch',
    'engine/third_party/symphonia-metadata-patches/0002-enforce-id3-metadata-budgets.patch',
    'engine/third_party/symphonia-core-patches/0003-propagate-embedded-metadata-policy.patch',
    'engine/third_party/symphonia-metadata-patches/0003-skip-unused-metadata.patch',
    'engine/third_party/symphonia-format-isomp4-patches/0002-skip-unused-metadata.patch',
    'engine/third_party/symphonia-format-isomp4-patches/0003-correct-sample-table-boundaries.patch',
    'engine/third_party/symphonia-format-ogg-patches/UPSTREAM',
    'engine/third_party/symphonia-format-ogg-patches/README.md',
    'engine/third_party/symphonia-format-ogg-patches/0001-propagate-metadata-policy.patch',
    'engine/third_party/symphonia-format-riff-patches/UPSTREAM',
    'engine/third_party/symphonia-format-riff-patches/README.md',
    'engine/third_party/symphonia-format-riff-patches/0001-honor-wave-metadata-policy.patch',
]
GENERATED = [
    'engine/target/debug/deps/runtime_v8', 'tools/artifact-manifest/target/release/artifact-manifest',
    'engine/jniLibs/full/arm64-v8a/libmigo.so',
    'engine/third_party/rusty_v8/x86_64-linux-gnu/librusty_v8.a',
    'engine/third_party/rusty_v8/x86_64-linux-gnu/gn_out/obj/librusty_v8.a',
    'engine/third_party/rusty_v8/aarch64-linux-gnu/librusty_v8.a',
    'engine/third_party/rusty_v8/x86_64-pc-windows-msvc/rusty_v8.lib',
    'platforms/android/library/build/outputs/aar/migo-full-debug.aar',
    'platforms/apple/Frameworks/MigoEngine.xcframework/Info.plist',
    'platforms/apple/.build/debug/objects.o', 'platforms/android/.gradle/cache.bin',
    'developer-docs/build/index.html', 'developer-docs/.astro/content.d.ts',
    'developer-docs/playwright-report/index.html', 'developer-docs/test-results/trace.zip',
    'scripts/lib/__pycache__/polling.cpython-312.pyc',
    'docs/implementation/2026-10-25-runtime-modernization/state/loop-state.json',
    'docs/implementation/2026-10-25-runtime-modernization/state/loop-current-session.log',
    'docs/implementation/2026-10-25-runtime-modernization/state/loop-config.before-20261030.json',
    'docs/implementation/2026-10-25-runtime-modernization/results/W09-producer/positive-debug-tests',
    'docs/implementation/2026-10-25-runtime-modernization/results/session.log',
    'dist/maven/migo.aar', 'local-certificate.pem',
    'tests/c_host/android-package-consumer/build/CMakeCache.txt',
    'core', 'core.1234', 'engine/core', 'engine/core.1234',
    'perf.data', 'perf.data.old', 'profile.cpuprofile', 'profile.heapsnapshot',
    'scripts/patch-helper.py.orig', 'scripts/patch-helper.py.rej',
]

REQUIRED.extend(['engine/crates/services/src/audio_http_spool.rs', 'engine/crates/services/src/audio_http_spool_tests.rs', 'engine/crates/audio/src/audio_thread_http_spool_tests.rs'])

REQUIRED.extend(['engine/crates/audio/src/streaming_http_control_tests.rs'])

# The streaming producer over upstream pull readers (P6).
REQUIRED.extend(['engine/third_party/minimp3-afb604c/LICENSE', 'engine/third_party/minimp3-afb604c/minimp3.h', 'engine/third_party/minimp3-afb604c/minimp3_ex.h', 'engine/crates/audio/src/stream_open.rs', 'engine/crates/audio/src/stream_producer.rs', 'engine/crates/audio/src/stream_producer_tests.rs', 'engine/crates/audio/src/pcm_source.rs', 'engine/crates/audio/src/pcm_source_tests.rs', 'engine/crates/audio/src/mp3_source.rs', 'engine/crates/audio/src/mp3_source_tests.rs', 'engine/crates/audio/src/media_source.rs', 'engine/crates/audio/src/media_source_tests.rs', 'engine/crates/audio/src/media_source_http.rs', 'engine/crates/audio/src/media_source_http_tests.rs', 'engine/crates/audio/src/test_media.rs', 'engine/crates/audio/native/mp3.c'])

def main():
    required = REQUIRED.copy()
    # The provenance gates check every path from their verified archive + patch
    # inventory. This fast gate checks the known broad-pattern exceptions.
    for name in ['deno_core-0.413.0', 'v8-150.4.0', 'ada-url-4.0.0', 'skia-bindings-0.157.0',
                 'tungstenite-0.30.0', 'cpal-0.18.2']:
        required.extend(f'engine/third_party/{name}/{file}'
                        for file in ['Cargo.lock', 'Cargo.toml.orig'])
    for name in required:
        if not (ROOT / name).is_file():
            raise ValueError(f'missing required source input: {name}')
    check(ROOT, required, GENERATED)
    # .gitignore alone does not stop a previously tracked/force-added report.
    # Source archives lack Git metadata; source boundaries still apply there.
    if (ROOT / '.git').exists():
        ignored = tracked_ignored(ROOT)
        if ignored:
            raise ValueError('ignored files still tracked; preserve local files and remove from index:\n'
                             + '\n'.join(ignored))
    print(f'Git ignore policy PASS: {len(required)} source paths / {len(GENERATED)} generated samples; main Git metadata untouched')

# Published source, notices and manifests are part of the selected product codec.
for folder in sorted((ROOT / 'engine/third_party').glob('symphonia-*-0.6.1')):
    REQUIRED.extend(str(p.relative_to(ROOT)) for p in sorted(folder.rglob('*')) if p.is_file())
REQUIRED.extend(str(p.relative_to(ROOT)) for p in sorted(
    (ROOT / 'engine/crates/audio/src/decoder/ogg-fixtures').iterdir()) if p.is_file())
# Every published DSP file is a required source/provenance input.
import json
for name, row in json.loads((ROOT / 'engine/third_party/rubato-sources.json').read_text()).items():
    folder=ROOT/'engine/third_party'/(name+'-'+row['version'])
    REQUIRED.extend(str(p.relative_to(ROOT)) for p in sorted(folder.rglob('*')) if p.is_file())

# AAC product codecs, provenance and complete immutable test inputs are source.
REQUIRED.extend(['engine/third_party/symphonia-aac-sources.json', 'scripts/testing/generate_aac_product_fixtures.py'])
for folder in ['engine/crates/audio/src/decoder/aac', 'engine/crates/audio/src/decoder/aac-fixtures']:
    REQUIRED.extend(str(p.relative_to(ROOT)) for p in sorted((ROOT/folder).rglob('*')) if p.is_file())

REQUIRED.extend(['engine/crates/services/src/audio_local_file.rs', 'engine/crates/services/src/audio_local_file_tests.rs'])


REQUIRED.extend(['engine/crates/services/src/network/http/range/borrowed_tests.rs'])

REQUIRED.extend(['engine/crates/audio/src/streaming_http_tests.rs'])

REQUIRED.extend(['engine/crates/audio/src/streaming_range_control_tests.rs'])

REQUIRED.extend(['engine/crates/audio/src/streaming_file_wave_tests.rs', 'engine/third_party/symphonia-format-riff-patches/0004-retained-pcm-source-seek.patch'])

if __name__ == '__main__':
    main()
