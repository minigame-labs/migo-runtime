# AAC-LC product fixtures

Original generated media from the W09 AAC candidate fixture generators, frozen
for ordinary offline product tests. No downloaded music is included.
`manifest.json` records 19 nonfragmented M4A layouts and three ADTS streams;
`sha256.json` records every encoded file, independent ADTS PCM and manifest.

The generator (`scripts/testing/generate_aac_product_fixtures.py`, FFmpeg based) went with the
step-driven decoders it shared code with; it remains in the history before 2fbef770. Encoded
bytes depend on the local FFmpeg build: these frozen bytes are not to be replaced silently.

M4A covers front/tail moov, reordered chunks, multiple mdat, co64, 17/9737/240137
presentation frames, sample-accurate edits beginning at 211/1537/1024 and v1 elst.
`valid-many.m4a` inserts 24 free atoms before the tail index; `edit-raw.m4a`
removes the presentation edit for independent trim comparisons.
PNS is disabled where exact layout/offset comparisons require identical AAC
state. The product does not promise arbitrary AAC seek bit identity.

`decoder::whole` tests decode every layout to its rate, channels and presentation length and
the ADTS streams to their independent references; the streaming tests compare the producer with
the whole-file decode at every seek, loop and rate. The
three `.f32le` files are complete independent FFmpeg decodes of the ADTS files;
ADTS has no container edit identifying encoder priming, so its declared decoded
length includes complete codec frames. It is not an exact source-WAV duration
oracle. M4A presentation follows the actual integer container timeline.
