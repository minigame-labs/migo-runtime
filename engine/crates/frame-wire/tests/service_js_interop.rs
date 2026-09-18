//! The service stream's two implementations against each other, in both
//! directions: the producer's JavaScript writes `MUS1` and this crate reads it;
//! this crate writes `MDS1` and the producer reads it.
//!
//! Both were written from the tables in `contracts/frame-wire/wire-v1.md`,
//! which is the right way to write them and no evidence that they agree. A
//! disagreement reaches a user as a storage write the host reads as a different
//! key, or a file read that answers with another request's bytes.
//!
//! `#[ignore]`d because they need a directory `cargo test` cannot produce;
//! `scripts/test-frame-wire-js-encoder.sh` is what guarantees they run.
//! `platforms/apple/WebContent/PerformancePlus/test/emit-service.mjs` holds the
//! same corpus, and each side asserts the other's length.

use std::{fs, path::PathBuf};

use frame_wire::service::{
    OwnedServiceRecord, ReplyError, ServiceDownRecord, ServiceRecord, down_envelope,
    encode_down_record, encode_service_message, read_service_message,
};
use frame_wire::value::OwnedValue;

/// Uplink messages: `(generation, sequence, records)`.
fn uplink_corpus() -> Vec<(u32, u64, Vec<OwnedServiceRecord>)> {
    use OwnedValue::*;
    vec![
        (
            1,
            1,
            vec![OwnedServiceRecord::Request {
                request_id: 1,
                op: 6,
                args: vec![Str("key".into())],
            }],
        ),
        (
            u32::MAX,
            (1 << 40) + 5,
            vec![OwnedServiceRecord::Command {
                op: 7,
                args: vec![
                    F64(-0.5),
                    U64((1 << 53) + 1),
                    I64(i64::MIN),
                    I32(-7),
                    U32(u32::MAX),
                    Bool(true),
                    Bool(false),
                    Null,
                ],
            }],
        ),
        (
            3,
            3,
            vec![
                OwnedServiceRecord::Request {
                    request_id: u32::MAX,
                    op: 11,
                    args: vec![
                        Bytes((0..=256u32).map(|b| b as u8).collect()),
                        Json("{\"a\":1}".into()),
                        Array(vec![U32(1), Array(vec![U32(64), U32(32)])]),
                    ],
                },
                OwnedServiceRecord::Cancel { request_id: 5 },
            ],
        ),
        (
            1,
            4,
            vec![
                OwnedServiceRecord::Request {
                    request_id: 2,
                    op: 1,
                    args: vec![Str("h\u{e9}llo \u{1F600}".into())],
                },
                OwnedServiceRecord::Request {
                    request_id: 3,
                    op: 1,
                    args: vec![Str(String::new()), Bytes(Vec::new())],
                },
            ],
        ),
    ]
}

/// Downlink messages: `(generation, records)`.
fn downlink_corpus() -> Vec<(u32, Vec<ServiceDownRecord>)> {
    use OwnedValue::*;
    vec![
        (
            7,
            vec![
                ServiceDownRecord::Reply {
                    request_id: 1,
                    outcome: Ok(Str("v".into())),
                },
                ServiceDownRecord::Reply {
                    request_id: 2,
                    outcome: Err(ReplyError {
                        class: "StorageError".into(),
                        message: "setStorage:fail data exceeds max size".into(),
                    }),
                },
            ],
        ),
        (
            7,
            vec![
                ServiceDownRecord::ReplyParked {
                    request_id: 3,
                    byte_length: 100,
                },
                ServiceDownRecord::Event {
                    event: 4,
                    values: vec![Str("x".into()), U32(5)],
                },
                ServiceDownRecord::Refused {
                    code: 4013,
                    sequence: 1 << 33,
                },
            ],
        ),
        (
            u32::MAX,
            vec![
                ServiceDownRecord::Reply {
                    request_id: 9,
                    outcome: Ok(Array(vec![U32(9), Array(vec![U32(640), U32(480)])])),
                },
                ServiceDownRecord::Reply {
                    request_id: 10,
                    outcome: Ok(Bytes((0..10).collect())),
                },
                ServiceDownRecord::Reply {
                    request_id: 11,
                    outcome: Ok(Json("{\"k\":[1]}".into())),
                },
                ServiceDownRecord::Reply {
                    request_id: 12,
                    outcome: Ok(U64(u64::MAX)),
                },
            ],
        ),
    ]
}

fn directory(variable: &str) -> PathBuf {
    PathBuf::from(
        std::env::var(variable)
            .unwrap_or_else(|_| panic!("{variable} must name the interop directory")),
    )
}

#[test]
#[ignore = "needs messages emitted by node; run through scripts/test-frame-wire-js-encoder.sh"]
fn service_messages_from_the_javascript_writer_are_read_unchanged() {
    let dir = directory("MIGO_SERVICE_IN_DIR");
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("the emitter's output directory must exist")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "bin"))
        .collect();
    entries.sort();
    let expected = uplink_corpus();
    assert_eq!(
        entries.len(),
        expected.len(),
        "the emitter wrote {} messages and this corpus has {}",
        entries.len(),
        expected.len()
    );
    for (path, (generation, sequence, records)) in entries.iter().zip(expected) {
        let bytes = fs::read(path).expect("reading an emitted message");
        // Byte for byte first: the reader accepting a message is weaker than the
        // producer writing the message the contract describes.
        assert_eq!(
            bytes,
            encode_service_message(generation, sequence, &records),
            "{} is not the bytes the reference writer produces",
            path.display()
        );
        let message = read_service_message(&bytes)
            .unwrap_or_else(|error| panic!("{} was refused: {error}", path.display()));
        assert_eq!(
            (message.generation, message.sequence),
            (generation, sequence)
        );
        let read: Vec<_> = message
            .records
            .iter()
            .map(ServiceRecord::to_owned_record)
            .collect();
        assert_eq!(
            read,
            records,
            "{} decoded to different records",
            path.display()
        );
    }
    println!("read {} JavaScript-encoded service messages", entries.len());
}

#[test]
#[ignore = "needs a directory prepared by the gate; run through scripts/test-frame-wire-js-encoder.sh"]
fn the_rust_writer_produces_service_answers_for_the_javascript_reader() {
    let out = directory("MIGO_SERVICE_OUT_DIR");
    fs::create_dir_all(&out).expect("the output directory must be creatable");
    let mut written = 0;
    for (index, (generation, records)) in downlink_corpus().into_iter().enumerate() {
        let mut bytes = down_envelope(generation).to_vec();
        for record in &records {
            bytes.extend_from_slice(&encode_down_record(record));
        }
        let (read_generation, read) =
            frame_wire::service::read_down_message(&bytes).expect("this crate reads what it wrote");
        assert_eq!((read_generation, read), (generation, records));
        fs::write(out.join(format!("answers-{index:03}.bin")), &bytes).expect("writing an entry");
        written += 1;
    }
    println!("wrote {written} service answer messages for the JavaScript reader");
}
