# Symphonia AAC 0.6.1 candidate patches

`UPSTREAM` pins the published crate archive. These patches are applied only in
the isolated W09 codec probe (`scripts/bench/audio-codecs.py --library-patches`).
The production Cargo workspace does not select this crate or a second codec stack.

`0001` makes the decoder honor `AudioDecoderOptions::gapless` with the existing
`AudioBuffer::trim` implementation, checks trim conversion/sums against the actual
decoded block, and clears output on failure through the existing error path.
`reset` also clears the exposed last output and restarts channel-pair PNS random
state, so replaying a stream from its beginning is deterministic.

The packet trim fields must be in PCM-frame units. The companion ISOMP4 candidate
patch normalizes its AAC track and packet timebase accordingly. Arbitrary AAC seek
with perceptual noise substitution does not promise the identical noise waveform
produced by decoding every earlier packet. The controlled waveform seek fixture
disables PNS; a separate PNS fixture verifies repeated complete playback/reset.

`scripts/testing/check_audio_codec_timeline.py` checks published-byte patch replay,
the actual Cargo-selected sources, real packet/decoder behavior and compiled
semantic mutations. Format acceptance, memory/performance and progressive MP3
remain W09 work; passing this probe is not production or device acceptance.

`0002` adds the optional `streaming` feature and a borrowed `AdtsPacketizer`.
It uses the existing ADTS header parser and shared track/signal validation;
Migo does not implement AAC or ADTS parsing. The supported decoder profile is
AAC-LC mono/stereo, one raw-data block per ADTS frame. The existing CRC header
field is parsed/skipped; this patch does not add CRC verification.

Incomplete input changes no packet timing. ID3 uses the metadata library
header/extent helper and bounded skip steps, with separate cumulative metadata
and synchronization quotas. First complete audio frames need no lookahead,
payloads borrow the caller input, format changes and partial frames fail.
The pull path shares timebase/max-packet initialization and signal checks;
it rejects a truncated final header and keeps bitrate duration estimates out
of exact num_frames. Errors in the push state machine are terminal.

The library/probe/compiled-control checkpoint is replayable with
`scripts/testing/check_audio_adts_streaming.py`. It is candidate evidence;
HTTP/DSP/player, product selection and platform/device acceptance remain open.
