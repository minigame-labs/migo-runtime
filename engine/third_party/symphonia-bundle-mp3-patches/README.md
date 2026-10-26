# Symphonia MPEG audio 0.6.1 candidate patch

Only the isolated W09 workspace applies these checksum-pinned patches. The
product manifest/lockfile and existing streaming decoder remain unchanged.

The optional, default-disabled `streaming` feature adds a push packetizer over
caller-owned contiguous input. It reuses the existing MPEG header and Xing,
LAME and VBRI parsing, plus the core PacketBuilder's borrowed packet/trimming
API. Pull and push readers share first-frame track initialization. The track
now has its sample timebase and maximum block size.

The pull reader's bitrate estimate remains a duration hint. It is no longer
advertised as an exact frame count or used for end trimming. An observed long
VBR input without Xing proved that the old estimate truncated actual PCM.

The push API consumes one borrowed frame, tag extent or synchronization step
per call. Incomplete input retains its bytes and advances no timing. ID3v2 uses
the metadata crate's existing header parser; ID3v1 has a fixed 128-byte extent.
Cumulative metadata/junk quotas prevent indefinite scanning. The first frame
is confirmed without probing over a live decoder's bit reservoir. EOF checks
truncated frames/tags and a known incomplete frame count; errors are terminal.

Current explicit unsupported cases include free bitrate, changing signal
format, midstream duration tags, and padding without an authoritative frame
count. The Migo prototype selects MP3 only. It bounds encoded buffering and
work per step, uses decode_ref, and supports cancellation and an optional total
quota independently of resident buffering. No blocking Read or per-stream OS
thread is introduced. The real HTTP/OffWorker/publication and resampling paths
have not yet switched to it; their acceptance must precede product selection.

`0002-handle-lightweight-bitstream-eof.patch` follows the matching core bit
reader patch: Layer3 handles standard UnexpectedEof for legal bit exhaustion,
while unrelated Other errors retain their failure semantics. The MP3 shared
adapter, HTTP/player and whole-corpus regressions cover the migration. Product
selection and native/device/performance acceptance remain separate.
