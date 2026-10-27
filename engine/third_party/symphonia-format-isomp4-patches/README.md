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

`0004-borrowed-aac-streaming-index.patch` provides an optional, default-disabled
streaming feature for bounded complete moov parsing and borrowed AAC-LC sample
descriptors. TrackState presentation/cursor logic is shared with the existing
pull reader. Sample timing keeps a reusable table cursor; sequential traversal
is linear in samples plus rows, without extra prefix arrays or per-packet
allocation. Backward lookup and segment changes reset the cursor. Chunk-start
offsets remain authoritative even in non-monotonic layouts.

The selected index profile is one nonfragmented AAC-LC mono/stereo track.
Defaults bound the encoded moov to 2MiB and each packet to 64KiB; complete atom
parsing is a bounded cold operation, not claimed to be a 4KiB parsing step.
Unused metadata is skipped before value allocation. Index descriptors preserve
absolute u64 offsets and optional object bounds, but callers must still validate
them against real mdat extents. The interface performs no IO, HTTP Range or
fallback, and is not yet a product decoder switch or streaming-playback claim.

`0005-borrowed-sequential-aac-packets.patch` adds bounded sequential delivery
over caller-owned input. Faststart movies collect only the bounded index;
packets borrow the input directly. NeedIndex and ReadAt are explicit requests
to the IO owner, not hidden blocking reads or an implemented HTTP client. The
caller must pin/verify the representation and actual mdat range before resuming.
Metadata skips are limited per call and cumulatively; the cold encoded index
buffer is released before packet playback. Object EOF and complete samples
remain distinct, including a known but unreceived non-audio tail.
The library protocol/codec gate is separate from common Migo/HTTP/Range/fallback
acceptance. The product still has no M4A codec/DSP switch.
