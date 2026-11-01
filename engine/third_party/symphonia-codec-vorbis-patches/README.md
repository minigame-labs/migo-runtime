# Symphonia Vorbis 0.6.1 patches

Product Vorbis selects this checksum-pinned source and its existing patches.
The other W09 codec/DSP ports remain candidates; platform/device and shipping
profile acceptance are tracked separately from the host regression checkpoint.

UPSTREAM pins the published archive. The selected Vorbis decoder is
patched alongside core: legal end-of-bitstream uses standard UnexpectedEof,
with no allocation for an error payload. Floor/residue retain their legal EOF
handling; unrelated Other errors no longer masquerade as normal exhaustion.
The Ogg/MP3/full-corpus gates verify the migration. Host regression acceptance does not establish native/device/performance acceptance.
