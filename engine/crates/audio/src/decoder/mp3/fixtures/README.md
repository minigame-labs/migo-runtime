The two MP3 files encode synthetic sine waves, generated with
`scripts/testing/generate_mp3_step_fixtures.py`. They exercise nonzero stereo
CBR and mono MPEG-2 VBR, including their complete encoded delay and tail.

The `.f32le` files are required test inputs: complete interleaved f32 PCM from
the pre-cursor minimp3 implementation. They are not build output or cache.
`source.json` records their hashes and the original source/program identity.
The generator deliberately leaves these reference files untouched.

Tests compare every sample against the references, independently of the new
cursor. Regeneration requires reviewing the compiler/encoder identity and the
full PCM change; do not bless a changed reference from the implementation under
test. No external recording or third-party audio is used.

The gapless music (480137 original stereo frames) and three tiny/MPEG-2
signals use required frozen i16le references from the unmodified pinned
minimp3_ex oracle. The music files moved here from the native candidate
fixture folder without changing bytes. `gapless-source.json` records the new
original-signal/encoder/oracle hashes; the native fixture sources.json retains
the music provenance. Acceptance does not regenerate these references.
