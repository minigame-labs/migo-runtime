# Symphonia Vorbis 0.6.1 patches

`UPSTREAM` pins the published crate. `scripts/test-symphonia-vendor-contract.sh` rebuilds the
copy from it and these patches, applied in order, and compares byte for byte. The audio crate
uses only the published reader and decoder APIs; these patches fix bugs, bound memory or remove
allocations inside them.

`0001-handle-lightweight-bitstream-eof.patch`: floor and residue decoding treat the core's
allocation-free `UnexpectedEof` (core patch 0005) as the legal end of a packet's bits.
