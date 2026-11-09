# Symphonia ISO MP4 0.6.1 patches

`UPSTREAM` pins the published crate. `scripts/test-symphonia-vendor-contract.sh` rebuilds the
copy from it and these patches, applied in order, and compares byte for byte. The audio crate
uses only the published reader and decoder APIs; these patches fix bugs, bound memory or remove
allocations inside them.

`0001-audio-presentation-trimming.patch`: an AAC-LC presentation window for nonfragmented audio
with no edit or one nonempty rate-one edit: track and packet timing in PCM frames, checked integer
time conversion, the edit intersected with the media duration, and priming and final padding
signalled as packet trims (priming packets still reach the decoder for overlap). Seek maps
presentation time back to media time with one block of preroll.

`0002-skip-unused-metadata.patch`: `MetaAtom` follows the format metadata policy (no revision when
disabled); the atom iterator still owns each skipped extent.

`0003-correct-sample-table-boundaries.patch`: the sample-to-chunk lookup returns the group that
begins exactly at a queried sample; time-to-sample arithmetic avoids overflow for large valid
indices and rejects an aggregate sample count beyond the u32 index domain.

`0004-pull-reader-chunk-order-timing-and-seek.patch`: a chunk's start offset is authoritative even
when the chunk lies before the previous one (the reader assumed ascending chunks and read past the
end of files laid out otherwise); sample timing advances through a cached time-to-sample cursor
instead of a lookup per sample; a seek resolves timing, extent and AAC preroll before moving the
cursor, so an out-of-range target leaves the reader where it was.
