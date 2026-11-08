# Symphonia Ogg 0.6.1 patches

Product Vorbis selects this checksum-pinned source and its existing patches.
The other W09 codec/DSP ports remain candidates; platform/device and shipping
profile acceptance are tracked separately from the host regression checkpoint.

`UPSTREAM` pins the published archive shared by product Vorbis and isolated audio probes.

Carry the format's explicit metadata policy into packet mappers at construction,
without adding options to the audio-packet call or changing LogicalStream.
Identified Vorbis/Opus comment packets return early in zero-retention mode,
avoiding comment-list walking, value/base64/chapter decoding and empty revision
publication. Retained comments use the library's option-aware reader. FLAC metadata mappings also skip optional comment/picture payloads
under that policy. No audio codec feature is enabled by this patch.

Ogg page/packet framing and codec setup remain mandatory. In particular, a
multi-page comment packet still uses the logical stream's existing reassembly
buffer and limit; skipping value parsing is not a zero-allocation Ogg claim.
The current acceptance fixtures cover the selected Vorbis audio profile; Opus,
FLAC, chained/multiplexed playback and their native/device profiles are not
claimed as accepted products.

`0002` adds the optional `streaming` feature. PagePacketizer borrows complete
CRC-verified pages; VorbisPacketizer supports one logical Vorbis stream with
strict sequence/continuation/EOS and explicit setup/audio/metadata budgets.
Complete packets borrow page input; spanning packets use two reusable buffers,
and unused comments are skipped without assembling their body. Vorbis detection,
setup, packet durations, channel mapping and granule trimming stay in the
library. Pull and push share header/lacing and packet timing arithmetic; the
streaming path uses a concrete Vorbis mapper. The feature does not select a
Migo product decoder, new CPU pool, blocking Reader or thread per stream.

The first integration checkpoint is library/typed-codec only. Chained and
multiplexed streams are explicitly rejected by this profile. Migo's shared
adapter and HTTP/DSP/player integration are separate acceptance steps.

`0003` adds optional pinned-length source requests and normalized presentation
metadata to the same Vorbis packetizer. It checks the final CRC page in the
caller's bounded window, restores the original pending first page, and replays
from byte zero with the existing packet durations and granule trimming. Ordinary
`new()` stays sequential. No second page parser or encoded working Vec is added;
seek refusal preserves state, and retained Vec capacities stay with their owner.
Migo services these requests through the original sealed File, CPU and ACK path.

`0004` waits for the original pending first audio page after a tail probe when
source restoration arrives in smaller chunks. The same page and packet locations
remain in place; no parsing, codec state or encoded buffer is duplicated. An
incomplete restored page at EOF is rejected before exposing a borrowed packet.
