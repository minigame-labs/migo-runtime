# AAC-LC product fixtures

Original generated media from the W09 AAC candidate fixture generators, frozen
for ordinary offline product tests. No downloaded music is included.
`manifest.json` records 19 nonfragmented M4A layouts and three ADTS streams;
`sha256.json` records every encoded file, independent ADTS PCM and manifest.

Regenerate into new directories with
`python3 scripts/testing/generate_aac_product_fixtures.py --work /tmp/aac-work --output /tmp/aac-inputs`.
The generator reuses `check_audio_complete_codecs.py`, `check_audio_mp4_index.py`
and `check_audio_mp4_source.py`. Generation logs and unused source WAV/candidate
files stay in the work directory. Encoded bytes depend on the local FFmpeg build;
do not silently replace these frozen bytes when FFmpeg changes.

M4A covers front/tail moov, reordered chunks, multiple mdat, co64, 17/9737/240137
presentation frames, sample-accurate edits beginning at 211/1537/1024 and v1 elst.
`valid-many.m4a` inserts 24 free atoms before the tail index; `edit-raw.m4a`
removes the presentation edit for independent trim comparisons.
PNS is disabled where exact layout/offset comparisons require identical AAC
state. The product does not promise arbitrary AAC seek bit identity.

The Rust tests compare every native sample against a separate mature pull
reader, then every converted sample against the existing whole-file SRC. The
three `.f32le` files are complete independent FFmpeg decodes of the ADTS files;
ADTS has no container edit identifying encoder priming, so its declared decoded
length includes complete codec frames. It is not an exact source-WAV duration
oracle. M4A presentation follows the actual integer container timeline.
