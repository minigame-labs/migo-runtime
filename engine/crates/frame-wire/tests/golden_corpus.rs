//! The fixed corpus both encoders are measured against.
//!
//! Not a round-trip test. A round trip proves the reader understands the
//! writer, which is true of any pair of implementations that share a mistake.
//! These are committed bytes: the JavaScript producer running inside WebContent
//! must emit exactly these for the same input, and any reader must accept or
//! reject each one exactly as `index.json` says.
//!
//! A diff here is a wire-format change. It is answered with a version decision,
//! not by regenerating the file.
//!
//! One case is a *rejected* one, and that is not decoration. A corpus with
//! nothing rejected in it never exercises the half of the reader that says no,
//! and the `accepted` column in the index would be a field no test reads.

use std::{fs, path::PathBuf};

use frame_wire::{
    FLAG_PRESENT, SECTION_KIND_COMMAND_STREAM, SECTION_KIND_DAMAGE, SECTION_KIND_INLINE_DATA,
    SECTION_KIND_RESOURCE_REFERENCES, SECTION_KIND_TIMING, WireError, builder::WireFrameBuilder,
    validate,
};

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../contracts/frame-wire/golden")
        .canonicalize()
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../contracts/frame-wire/golden")
        })
}

/// What a reader must do with a case.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Expect {
    Accepted,
    Rejected(WireError),
}

/// One section of a case's input.
struct SectionSpec {
    kind: u32,
    item_count: u32,
    payload: Vec<u8>,
}

/// A case, as an *input specification* plus the verdict a reader must reach.
///
/// The bytes are derived from this, never written beside it. The corpus exists
/// so two independent encoders can be checked against one fixed artefact, and
/// the second encoder is JavaScript running inside WebContent -- which cannot
/// read this file. So the specification travels in `index.json` and both
/// encoders build from it. A corpus whose inputs live only in the Rust test is
/// a corpus only Rust can be checked against, which is half a contract.
struct Case {
    name: &'static str,
    launch_nonce: u128,
    sequence: u64,
    runtime_generation: u64,
    surface_generation: u64,
    resource_epoch: u64,
    frame_id: u32,
    flags: u32,
    sections: Vec<SectionSpec>,
    /// A deliberate departure from the canonical encoding, for a case whose
    /// point is that a reader rejects it. Named so the other encoder knows it
    /// is *expected* not to reproduce these bytes, rather than silently
    /// disagreeing with the corpus.
    deviation: Option<&'static str>,
    expect: Expect,
}

impl Case {
    /// The bytes, built from the specification. One code path, so the committed
    /// artefact and the published input cannot describe different packets.
    fn encode(&self) -> Vec<u8> {
        let mut builder = WireFrameBuilder::new();
        builder.launch_nonce = self.launch_nonce;
        builder.sequence = self.sequence;
        builder.runtime_generation = self.runtime_generation;
        builder.surface_generation = self.surface_generation;
        builder.resource_epoch = self.resource_epoch;
        builder.frame_id = self.frame_id;
        builder.flags = self.flags;
        match self.deviation {
            Some("extra_gap_8") => builder.extra_gap = 8,
            Some(other) => panic!("unknown deviation {other}"),
            None => {}
        }
        for section in &self.sections {
            builder = builder.section(section.kind, section.item_count, &section.payload);
        }
        builder.build()
    }

    /// The input, as it appears in `index.json`.
    ///
    /// The four wide fields are decimal *strings*. JSON numbers are IEEE
    /// doubles once a JavaScript reader touches them, and `all-section-kinds`
    /// carries values well past 2^53 precisely so a truncating reader on either
    /// side fails here rather than on a device. Strings survive; `BigInt` reads
    /// them back exactly.
    fn input_json(&self) -> String {
        let sections: Vec<String> = self
            .sections
            .iter()
            .map(|section| {
                format!(
                    "{{ \"kind\": {}, \"item_count\": {}, \"payload_hex\": \"{}\" }}",
                    section.kind,
                    section.item_count,
                    hex(&section.payload)
                )
            })
            .collect();
        let deviation = match self.deviation {
            Some(name) => format!(", \"deviation\": \"{name}\""),
            None => String::new(),
        };
        format!(
            "{{ \"launch_nonce\": \"{}\", \"sequence\": \"{}\", \"runtime_generation\": \"{}\",              \"surface_generation\": \"{}\", \"resource_epoch\": \"{}\", \"frame_id\": {},              \"flags\": {}, \"sections\": [{}]{} }}",
            self.launch_nonce,
            self.sequence,
            self.runtime_generation,
            self.surface_generation,
            self.resource_epoch,
            self.frame_id,
            self.flags,
            sections.join(", "),
            deviation
        )
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn section(kind: u32, item_count: u32, payload: Vec<u8>) -> SectionSpec {
    SectionSpec {
        kind,
        item_count,
        payload,
    }
}

/// Deterministic by construction: no timestamps, no randomness, no allocator
/// addresses. A corpus that changes between runs cannot be committed.
fn cases() -> Vec<Case> {
    let stream: Vec<u8> = (0..64u8).collect();
    let inline: Vec<u8> = (0..21u8).map(|value| value.wrapping_mul(7)).collect();
    let resources: Vec<u8> = (0..12u8).map(|value| value.wrapping_add(200)).collect();
    let damage: Vec<u8> = (0..16u8).map(|value| value.wrapping_mul(3)).collect();
    let more: Vec<u8> = (0..32u8).rev().collect();
    let timing: Vec<u8> = (0..12u8).map(|value| value.wrapping_mul(11)).collect();

    vec![
        // The smallest legal packet: one command stream, present, nothing else.
        Case {
            name: "minimal-present",
            launch_nonce: 0,
            sequence: 1,
            runtime_generation: 1,
            surface_generation: 0,
            resource_epoch: 0,
            frame_id: 1,
            flags: FLAG_PRESENT,
            sections: vec![section(SECTION_KIND_COMMAND_STREAM, 0, Vec::new())],
            deviation: None,
            expect: Expect::Accepted,
        },
        // A frame shaped like a real one: commands, an inline blob whose length
        // is not a multiple of 8 (so the encoder's padding is pinned, and so is
        // the rule that pad bytes are zero), resources at their exact record
        // width, and an advisory damage section at its own.
        Case {
            name: "typical-frame",
            launch_nonce: 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210,
            sequence: 42,
            runtime_generation: 3,
            surface_generation: 9,
            resource_epoch: 2,
            frame_id: 41,
            flags: FLAG_PRESENT,
            sections: vec![
                section(SECTION_KIND_COMMAND_STREAM, 16, stream),
                section(SECTION_KIND_INLINE_DATA, 21, inline.clone()),
                section(SECTION_KIND_RESOURCE_REFERENCES, 3, resources.clone()),
                section(SECTION_KIND_DAMAGE, 1, damage.clone()),
            ],
            deviation: None,
            expect: Expect::Accepted,
        },
        // Every section kind at once, with the wide identity and timeline
        // fields at values a 32-bit, 53-bit or 64-bit truncation would corrupt.
        // This is the case that notices an encoder writing the header at the
        // wrong width -- including a JavaScript one that reached for `Number`.
        Case {
            name: "all-section-kinds",
            launch_nonce: u128::MAX - 5,
            sequence: 0x0000_0001_0000_0000,
            runtime_generation: 0x8000_0000_0000_0001,
            surface_generation: 0x7FFF_FFFF_FFFF_FFFF,
            resource_epoch: 0x0000_00FF_0000_00FF,
            frame_id: 0xFFFF_FFFF,
            flags: FLAG_PRESENT,
            sections: vec![
                section(SECTION_KIND_COMMAND_STREAM, 8, more.clone()),
                section(SECTION_KIND_INLINE_DATA, 21, inline),
                section(SECTION_KIND_RESOURCE_REFERENCES, 3, resources),
                section(SECTION_KIND_DAMAGE, 1, damage),
                section(SECTION_KIND_TIMING, 12, timing),
            ],
            deviation: None,
            expect: Expect::Accepted,
        },
        // A barrier: `flags == 0`. The same bytes as a presenting packet but for
        // that one field and the checksum, so an encoder that ignored the flag
        // -- or a reader that refused the packet, as v1 once did -- disagrees
        // with the corpus here and nowhere else.
        Case {
            name: "barrier",
            launch_nonce: 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210,
            sequence: 7,
            runtime_generation: 3,
            surface_generation: 9,
            resource_epoch: 2,
            frame_id: 6,
            flags: 0,
            sections: vec![section(
                SECTION_KIND_COMMAND_STREAM,
                8,
                (0..32u8).map(|value| value.wrapping_mul(5)).collect(),
            )],
            deviation: None,
            expect: Expect::Accepted,
        },
        // Well formed in every respect except that its section does not start
        // where the canonical layout puts it. Aligned, in order, in bounds,
        // checksum correct -- and rejected, because the eight bytes of gap are
        // inside the integrity check and outside every consumer.
        Case {
            name: "rejected-noncanonical-gap",
            launch_nonce: 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210,
            sequence: 43,
            runtime_generation: 3,
            surface_generation: 0,
            resource_epoch: 0,
            frame_id: 42,
            flags: FLAG_PRESENT,
            sections: vec![section(SECTION_KIND_COMMAND_STREAM, 8, more)],
            deviation: Some("extra_gap_8"),
            expect: Expect::Rejected(WireError::SectionNotCanonical),
        },
    ]
}

fn sha256_hex(bytes: &[u8]) -> String {
    // A small, self-contained SHA-256 so the corpus check does not pull a
    // dependency into a crate whose whole point is a small trust boundary.
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = bytes.to_vec();
    let bit_length = (bytes.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    for chunk in message.chunks(64) {
        let mut w = [0u32; 64];
        for (index, word) in chunk.chunks(4).enumerate() {
            w[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }

    h.iter().map(|word| format!("{word:08x}")).collect()
}

#[test]
fn the_committed_corpus_matches_this_encoder() {
    let dir = corpus_dir();
    let updating = std::env::var_os("MIGO_UPDATE_FRAME_WIRE_GOLDEN").is_some();
    if updating {
        fs::create_dir_all(&dir).expect("create corpus directory");
    }

    let mut index_lines = Vec::new();
    let mut mismatches = Vec::new();

    for case in cases() {
        let path = dir.join(format!("{}.bin", case.name));
        let bytes = case.encode();
        let digest = sha256_hex(&bytes);
        let verdict = match case.expect {
            Expect::Accepted => "\"accepted\": true".to_string(),
            Expect::Rejected(error) => format!(
                "\"accepted\": false, \"wire_error\": {}, \"wire_error_name\": \"{:?}\"",
                error.code(),
                error
            ),
        };
        index_lines.push(format!(
            "    {{ \"name\": \"{}\", \"bytes\": {}, \"sha256\": \"{}\", {}, \"input\": {} }}",
            case.name,
            bytes.len(),
            digest,
            verdict,
            case.input_json()
        ));

        if updating {
            fs::write(&path, &bytes).expect("write corpus case");
            continue;
        }

        match fs::read(&path) {
            Ok(committed) => {
                if committed != bytes {
                    mismatches.push(format!(
                        "{}: committed {} bytes, encoder produced {}",
                        case.name,
                        committed.len(),
                        bytes.len()
                    ));
                }
                // The committed bytes must get the committed verdict. Checking
                // the encoder's output instead would let a corpus file rot
                // while the test kept passing on freshly built bytes.
                match (validate(&committed), case.expect) {
                    (Ok(_), Expect::Accepted) => {}
                    (Err(error), Expect::Rejected(expected)) if error == expected => {}
                    (Ok(_), Expect::Rejected(expected)) => mismatches.push(format!(
                        "{}: the index says rejected with {expected:?}, the reader accepted it",
                        case.name
                    )),
                    (Err(error), Expect::Accepted) => mismatches.push(format!(
                        "{}: the index says accepted, the reader rejected it with {error:?}",
                        case.name
                    )),
                    (Err(error), Expect::Rejected(expected)) => mismatches.push(format!(
                        "{}: the index says rejected with {expected:?}, the reader said {error:?}",
                        case.name
                    )),
                }
            }
            Err(error) => mismatches.push(format!("{}: unreadable ({error})", case.name)),
        }
    }

    if updating {
        let index = format!("{{\n  \"cases\": [\n{}\n  ]\n}}\n", index_lines.join(",\n"));
        fs::write(dir.join("index.json"), index).expect("write corpus index");
        panic!(
            "corpus regenerated. Re-run without MIGO_UPDATE_FRAME_WIRE_GOLDEN, and treat the diff \
             as a wire-format change needing a version decision."
        );
    }

    assert!(
        mismatches.is_empty(),
        "the encoder no longer produces the committed bytes:\n  {}",
        mismatches.join("\n  ")
    );

    let index = fs::read_to_string(dir.join("index.json")).expect("corpus index");
    for line in &index_lines {
        let trimmed = line.trim().trim_end_matches(',');
        assert!(
            index.contains(trimmed),
            "index.json is missing or stale for this case:\n  {trimmed}"
        );
    }
}

/// The corpus is worth nothing if it is empty, and an empty directory would
/// otherwise report a clean pass. It is also worth less than it looks if every
/// case is an accepted one.
#[test]
fn the_corpus_covers_more_than_one_shape_and_both_verdicts() {
    let cases = cases();
    assert!(
        cases.len() >= 4,
        "the corpus must cover more than one shape"
    );
    assert!(
        cases
            .iter()
            .any(|case| matches!(case.expect, Expect::Rejected(_))),
        "a corpus with nothing rejected never exercises the reader saying no"
    );
    assert!(
        cases
            .iter()
            .any(|case| matches!(case.expect, Expect::Accepted)),
        "a corpus with nothing accepted proves only that the reader refuses everything"
    );
}
