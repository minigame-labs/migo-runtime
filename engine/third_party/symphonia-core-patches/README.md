# Symphonia core 0.6.1 candidate patch

The product does not select this crate. `UPSTREAM` pins the published source;
only the isolated W09 probes apply this patch.

`PacketBuilder::trimmed_dur` computed local trim fields but calculated valid
duration from the previous, usually zero, fields. A real MP3 packet decoded to
47 valid frames while advertising duration 1152. The patch calculates duration
from the new trims and clamps their sum to the decoded block length. i128
intermediates represent every i64 PTS plus u64 duration without Timestamp
overflow, including entirely discarded blocks and numeric endpoints.

The actual MP3 decoder contract first reproduced the failure. Published packet
module tests are compiled directly with the selected core units, without adding
core's unrelated rand dev-dependency to the probe. Owned/borrowed packet cases,
explicit trim replacement, and compiled mutations cover duration, tail clamp
and signed overflow. The product migration and device checks remain separate.

`0002-enforce-id3-metadata-budgets.patch` adds independent metadata revision,
encoded-frame scratch and nesting limits, plus conservative retained-storage
accounting (including capacities; identical raw/standard String Arcs counted
once). These new limits are currently enforced by the ID3v2 reader, not claimed
for every format. They are additive to the existing per-tag/per-visual soft
limits. Allocator bookkeeping is outside the accounting.

With `limit_total_bytes=Maximum(0)`, the external metadata probe also avoids
storing empty revisions or chapter side data after a reader returns. Individual
readers must still implement their own parsing/scratch policy; this probe check
alone does not prevent allocations inside other metadata formats.

`0003-propagate-embedded-metadata-policy.patch` adds explicit embedded metadata
options to FormatOptions. Probe copies its metadata argument into that field
before constructing a container reader. Direct callers can use the builder;
the explicit Probe argument takes precedence. Readers must adopt the policy;
adding the field does not imply every container now enforces every quota.

`0004-keep-profile-plane-views-on-stack.patch` keeps temporary planar audio
views for the selected bounded channel profile on the stack. The PCM decoder
and shared streaming probes cover the change without expanding product features.

`0005-lightweight-bitstream-eof.patch` uses the standard allocation-free
UnexpectedEof error for bit exhaustion. Legal Vorbis floor/residue and MP3
Layer3 consumers are migrated in their matching candidate patches. A real
2200-second silent Vorbis stream exposed repeated allocation of the previous
string-carrying error. Precise EOF classification, allocation checks, the Core
suite and the complete codec corpus cover the change; unrelated IO errors do
not become normal exhaustion. This remains a candidate, not a product cutover.

`0006-borrowed-slice-buffered-seek.patch` makes the existing borrowed BufReader
implement SeekBuffered over its real slice. Absolute and relative positions
clamp to the actual buffer; even isize::MIN/u64::MAX do not overflow. This lets
the regular MP4 atom parser read a complete bounded moov without another ring
buffer or a payload copy. Oversized skips are checked before a usize cast.
Core slice-position/skip tests and the actual indexed AAC decoder cover it.
