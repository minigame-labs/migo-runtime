# Symphonia RIFF 0.6.1 product and candidate patches

`UPSTREAM` pins the published archive. The whole-file WAVE product and isolated
candidates select this same patched source.

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

The borrowed interface is selected for whole-file product WAVE decoding; this
does not establish product WAVE streaming delivery.
HTTP/DSP/player integration, format-policy/seek/rate and device acceptance remain
separate acceptance requirements. The separate PCM oracle fixture lives
in scripts/testing/fixtures/audio-codec-probe/wave_pcm_contracts.rs.

`0003` keeps the product PCM profile compatible with classic 1–32 channel
files and legacy LSB-aligned 24-in-32 storage. The existing PCM decoder performs
the conversion using its established sample-shift parameter; no alternate
parser or converter is added. Extensible zero valid bits retain their stored
width; valid left-aligned extensible PCM keeps its canonical container scale.
Discrete channel layouts preserve all interleaved samples without inventing
speaker positions. AIFF constructors keep their existing zero shift. Two
library regressions and independent product numeric/legacy comparisons cover
these decisions. Float64, companding and ADPCM remain outside the product
profile. All features (including the unselected AIFF code) must still compile.

`0004` adds a bounded PCM byte-offset seek/resume port to the same incremental
packetizer. It retains the original known data extent and uses PacketInfo's
existing packet alignment; exact pending-read validation fences resume. Unknown
extents remain unseekable, invalid targets preserve the old cursor and pending
request, and returned parser/resume failures remain terminal. The product reuses
its existing codec reset, bounded encoded history, global SRC windows, sealed
File capability, original CPU, and revision/consumer ACK handshake. This port
does not add a second RIFF parser, decoder or resource budget.
