# Symphonia PCM 0.6.1 patches

`UPSTREAM` pins the published crate. `scripts/test-symphonia-vendor-contract.sh` rebuilds the
copy from it and these patches, applied in order, and compares byte for byte. The audio crate
uses only the published reader and decoder APIs; these patches fix bugs, bound memory or remove
allocations inside them.

`0001-decode-exact-pcm-packet-frames.patch`: the decoder rendered to the reusable buffer's capacity
and swallowed the resulting EOF error, so every packet shorter than the largest one paid for an
error allocation. It now renders exactly the packet's frames, propagates rendering errors and
rejects a packet ending inside a coded frame.
