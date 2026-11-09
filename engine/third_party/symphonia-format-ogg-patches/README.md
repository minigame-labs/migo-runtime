# Symphonia Ogg 0.6.1 patches

`UPSTREAM` pins the published crate. `scripts/test-symphonia-vendor-contract.sh` rebuilds the
copy from it and these patches, applied in order, and compares byte for byte. The audio crate
uses only the published reader and decoder APIs; these patches fix bugs, bound memory or remove
allocations inside them.

`0001-propagate-metadata-policy.patch`: the reader hands `FormatOptions::metadata_options` to its
stream mappers, so Vorbis (and FLAC) comment and picture parsing in Ogg follows the same limits.
