# Symphonia AAC 0.6.1 patches

`UPSTREAM` pins the published crate. `scripts/test-symphonia-vendor-contract.sh` rebuilds the
copy from it and these patches, applied in order, and compares byte for byte. The audio crate
uses only the published reader and decoder APIs; these patches fix bugs, bound memory or remove
allocations inside them.

`0001-audio-presentation-trimming.patch`: the decoder honours `AudioDecoderOptions::gapless` with
the existing `AudioBuffer::trim`, checks trims against the decoded block, and `reset` clears the
last output and restarts the channel-pair PNS random state, so replaying from the start is
deterministic. Trims are in PCM frames (the MP4 patches normalize the timebase accordingly).

`0002-adts-reader-track-and-eof.patch`: the ADTS reader's track carries what the decoder and the
presentation clock need (AAC-LC only, mono or stereo, a PCM-frame timebase, 1024 frames per
packet); a changed signal format mid-stream is refused; a header cut short by EOF is told apart
from a clean end; a timestamp overflow is an error rather than a silent end.
