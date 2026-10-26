# Symphonia metadata 0.6.1 candidate patch

`UPSTREAM` pins the published crate. The product does not select the candidate.

Expose a narrow `Id3v2Reader::total_tag_len` query that delegates to the existing
ID3 header parser. It returns the header, payload and optional footer extent
without reading or allocating the tag body. This lets an incremental audio
packetizer skip bounded opaque metadata rather than searching its payload for
false MPEG sync words. No ID3 size/version parser is copied into Migo.

The push packetizer enforces cumulative metadata/junk limits independently of
the full metadata decoder. This patch does not claim that all existing metadata
allocation limits or malformed-input behavior have been verified. Full heap,
fuzz and corpus acceptance remains W09 work.

`0002-enforce-id3-metadata-budgets.patch` makes ID3v2 honor decoded per-tag and
picture limits, bounds encoded scratch before allocation, accounts retained
revision/chapter storage and bounds both nested frames and the TOC graph. Zero
retention skips the optional body without decoding it. Oversized optional
frames are skipped, not turned into an audio failure. Existing v2.3/v2.4
compressed-frame skipping is retained; no decompressor dependency is added.
The reader consumes and validates the v2.4 footer. Embedded RIFF ID3 uses the
new defaults; propagation of explicit format options, and other metadata
formats, remain separate acceptance work.

Frame body reads share a finite-extent helper: the existing ScopedStream and
BufReader checks suffice for plain data, while UnsyncStream's generic boxed
reader otherwise allocates before checking its inner extent. UnsyncStream also
skips decoded bytes through its own reader, preserving the following frame when
stuffed zero bytes occur. No container/parser implementation is added to Migo.

`0003-skip-unused-metadata.patch` extends zero-retention paths to ID3v1, APE and
embedded Vorbis comments. APE items use the existing ScopedStream to prevent
values crossing into the footer/media; retained parsing rejects leftover item
bytes, and skipped values remain opaque. Correct the APEv2 no-footer flag,
handle optional footers, and keep structural/header/footer validation active.
ID3v2.2 compression remains unsupported for retained parsing, but its encoded
extent can be skipped without introducing a decompressor. Nonzero-retention
quotas outside ID3v2 remain separate work.
