# Symphonia RIFF 0.6.1 patches

`UPSTREAM` pins the published crate. `scripts/test-symphonia-vendor-contract.sh` rebuilds the
copy from it and these patches, applied in order, and compares byte for byte. The audio crate
uses only the published reader and decoder APIs; these patches fix bugs, bound memory or remove
allocations inside them.

`0001-honor-wave-metadata-policy.patch`: a WAVE `LIST INFO` chunk is parsed only when
`FormatOptions::metadata_options` allows metadata, and is skipped by its extent otherwise.

`0003-preserve-pcm-storage-and-discrete-channels.patch`: classic 1-32 channel files and legacy
right-aligned 24-in-32 storage decode correctly: block alignment gives the stored width, the PCM
decoder's existing sample shift does the conversion, and discrete layouts keep every interleaved
sample without inventing speaker positions.
