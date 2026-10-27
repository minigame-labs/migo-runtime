# Symphonia ISOMP4 0.6.1 candidate patches

`UPSTREAM` pins the published crate archive. Only the isolated W09 workspace
selects these patches; the product manifest and lockfile remain unchanged.
No extra MP4 parser is added to Migo. The patch uses the library's existing
`elst`, `stts`, media header and AudioSpecificConfig parsers.

`0001` implements an AAC-LC presentation window for nonfragmented audio with no
edit or a single nonempty rate-one edit. It normalizes track/packet timing to PCM
frames, uses integer checked time conversion, intersects the edit with the media
duration, and signals priming and final codec-block padding using packet trims.
It still passes priming packets to the decoder to establish overlap state. Seek
maps the presentation time back to media time and supplies one block of preroll.
Terminal EOF preserves the existing atom iterator's raw-read anchor, allowing a
later backward seek rather than losing the pending mdat.

The edit duration uses the movie timescale while media_time uses the media
timescale. The candidate retains complete frames within the declared edit and
clamps its end to available media. A coarse movie timescale can therefore yield
fewer frames than the original encoder input: it does not invent missing
precision or hardcode a 1024-frame encoder delay. Exact sample-rate movie timing
and nonstandard edit offsets are tested independently.

Current explicit unsupported cases: fragmented AAC, multiple/empty/rate-changing
edits, edits on other codecs, and media boundaries that cannot be represented
exactly as PCM frames. These return errors, not successful unedited output.
This is deliberately a limited candidate, not a claim of complete ISOBMFF edit
support. Broader acceptance remains a product decision and W09 follow-up.

`scripts/testing/check_audio_codec_timeline.py` performs actual container surgery
in test fixtures only, checks real decode/seek results and source mutants, and
verifies checksum-pinned patch replay and Cargo source selection. Native/device
and performance approval remain separate.

`0002-skip-unused-metadata.patch` carries the format metadata policy on the
existing hierarchical AtomIterator. MetaAtom returns no revision in disabled
mode; iterator bounds/skip behavior still owns its encoded extent. Top-level
and moov/udta metadata share the policy. Codec sample descriptions, edit lists,
seek tables and media data remain on their existing paths. No second MP4 parser
or decoder implementation is added to Migo.

`0003-correct-sample-table-boundaries.patch` makes the sample-to-chunk lookup
return the group beginning exactly at a queried sample. At that boundary the
old group's arithmetic could still produce the right byte offset; this fix
does not assert previously corrupted PCM. It restores the lookup's contract
before exposing index access to streaming callers. Time-to-sample arithmetic
also avoids a debug-build overflow for valid large sample indices and rejects
an aggregate sample count exceeding the u32 index domain while parsing.
Three executable boundary contracts include exact u32 limits and actual atom
parsing. This patch does not implement incremental M4A, HTTP Range or fallback.
