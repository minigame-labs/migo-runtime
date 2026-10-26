# Symphonia Vorbis 0.6.1 candidate patch

UPSTREAM pins the published archive. This existing candidate decoder is now
patched alongside core: legal end-of-bitstream uses standard UnexpectedEof,
with no allocation for an error payload. Floor/residue retain their legal EOF
handling; unrelated Other errors no longer masquerade as normal exhaustion.
The Ogg/MP3/full-corpus gates verify the migration. This is not product or
native/device/performance acceptance.
