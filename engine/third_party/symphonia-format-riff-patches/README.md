# Symphonia RIFF 0.6.1 candidate patch

`UPSTREAM` pins the published archive. Only isolated audio candidates select the
patched source; the product dependency graph has not switched.

WavReader checks the explicit metadata policy before parsing optional INFO
contents. The existing chunk reader still validates outer lengths and handles
padding; fmt/fact/data parsing and sample timing remain active.

The published reader collected INFO revisions in a local log and then discarded
that log when constructing the reader. Preserve it, appending after external
revisions, so the ordinary retention path remains useful. The zero-retention
path publishes no empty revision.

This patch covers selected WAVE INFO. AIFF, embedded RIFF ID3 option propagation,
nonzero tag/picture/revision quotas and native/device acceptance remain separate.


`streaming` (implies `wav`) adds a borrowed incremental PCM packetizer. It uses
shared RIFF/WAVE prefix and chunk-envelope parsing, plus the existing fmt/fact
and PCM packet parameters. It never retries a partially mutated blocking reader.
Complete PCM frames can be emitted before the remaining data extent arrives;
optional metadata is discarded in at most 4096-byte steps. Format headers and
packets have separate working-set limits; encoded/audio track length is not
silently treated as resident memory. ADPCM is outside this incremental profile.

The shared fmt reader widens block-alignment arithmetic before validating large
channel counts. Channel-mask correction uses a wider intermediate and returns a
normal error when added positions do not fit, rather than overflowing a shift
or truncating channel positions. These changes apply to pull and push readers.

The patch is a candidate library interface, not product WAVE streaming delivery.
HTTP/DSP/player integration, format-policy/seek/rate and device acceptance remain
required before the product codec switch. The separate PCM oracle fixture lives
in scripts/testing/fixtures/audio-codec-probe/wave_pcm_contracts.rs.
