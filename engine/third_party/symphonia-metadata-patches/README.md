# Symphonia metadata 0.6.1 patches

`UPSTREAM` pins the published crate. `scripts/test-symphonia-vendor-contract.sh` rebuilds the
copy from it and these patches, applied in order, and compares byte for byte. The audio crate
uses only the published reader and decoder APIs; these patches fix bugs, bound memory or remove
allocations inside them.

`0001-borrowed-incremental-framing.patch`: `Id3v2Reader::total_tag_len`, the header, payload and
optional footer extent of a tag, read from its header alone. Format detection skips ID3 tags with
it before telling ADTS from MP3, without parsing or allocating the tag body.

`0002-enforce-id3-metadata-budgets.patch`: ID3v2 honours the per-tag and picture limits, bounds
encoded scratch before allocating, accounts retained revision/chapter storage, bounds nested frames
and the TOC graph, and skips an oversized optional frame instead of failing the audio.

`0003-skip-unused-metadata.patch`: zero-retention paths for ID3v1, APE and embedded Vorbis comments
(values skipped opaquely within their extents), with the APEv2 footer flags corrected.
