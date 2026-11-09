# Symphonia core 0.6.1 patches

`UPSTREAM` pins the published crate. `scripts/test-symphonia-vendor-contract.sh` rebuilds the
copy from it and these patches, applied in order, and compares byte for byte. The audio crate
uses only the published reader and decoder APIs; these patches fix bugs, bound memory or remove
allocations inside them.

`0001-consistent-packet-trim-duration.patch`: `PacketBuilder::trimmed_dur` computed the valid
duration from the previous, usually zero, trim fields, so a packet decoding to 47 valid frames
advertised 1152. Duration now follows the new trims, their sum is clamped to the decoded block, and
i128 intermediates keep every i64 PTS plus u64 duration from overflowing.

`0002-enforce-id3-metadata-budgets.patch`: metadata revision, encoded-frame scratch and nesting
limits with conservative retained-storage accounting, enforced by the ID3v2 reader, in addition to
the per-tag and per-visual limits.

`0003-propagate-embedded-metadata-policy.patch`: `FormatOptions::metadata_options`, so a reader
constructed directly (as the audio crate does) honours the same tag and picture limits as Probe.

`0004-keep-profile-plane-views-on-stack.patch`: temporary planar views for the bounded channel
profile stay on the stack instead of being allocated per decode.

`0005-lightweight-bitstream-eof.patch`: bit exhaustion reports the allocation-free `UnexpectedEof`
instead of a string-carrying error; a long silent Vorbis stream allocated one per packet.
