//! Reuse the real service host fixture; only the native producer is controlled.
use super::*;
use shared::audio_analyser_samples::{AnalyserSamples, AnalyserStorage};
struct Native<T> {
    data: Vec<T>,
    _permit: shared::audio_physical::AudioAggregatePermit,
    dropped: Arc<AtomicBool>,
}
impl<T: Send> AnalyserStorage<T> for Native<T> {
    fn samples(&self) -> &[T] {
        &self.data
    }
    fn samples_mut(&mut self) -> &mut [T] {
        &mut self.data
    }
}
impl<T> Drop for Native<T> {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}
const OPS: [u32; 4] = [
    id::op_audio_analyser_byte_time_domain,
    id::op_audio_analyser_byte_frequency,
    id::op_audio_analyser_float_time_domain,
    id::op_audio_analyser_float_frequency,
];
fn byte_values(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}
fn float_values(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            f32::from_bits(match i {
                0 => 0x80000000,
                1 => 0x7fc00003,
                2 => 0x7f800000,
                _ => (-(i as f32) * 0.125).to_bits(),
            })
        })
        .collect()
}
fn native<T: Send + 'static>(
    f: &Fixture,
    data: Vec<T>,
    dropped: Arc<AtomicBool>,
) -> AnalyserSamples<T> {
    let bytes = data.capacity() * std::mem::size_of::<T>();
    AnalyserSamples::new(Native {
        data,
        _permit: f.ledger.try_reserve(bytes, "native analyser").unwrap(),
        dropped,
    })
}
fn request(f: &Fixture, op: u32) -> shared::protocol::audio_cmd::AudioCmd {
    let _enter = f.runtime.enter();
    if std::env::var("MIGO_ANALYSER_WIRE_FAULT").as_deref() == Ok("legacy") {
        // Select the actual original serializer/outbox route in the counterfactual test only.
        let future = f
            .host
            .context
            .call_async(op, vec![OwnedValue::U32(20)], &f.dispatcher.render)
            .unwrap();
        let outbox = f.host.outbox.clone();
        let in_flight = f.dispatcher.in_flight.clone();
        let mut map = in_flight.lock();
        let completion = in_flight.clone();
        let task = tokio::spawn(async move {
            let result = future.await;
            if completion.lock().remove(&5).is_some() {
                outbox.reply(5, result);
            }
        });
        map.insert(5, task.abort_handle());
    } else {
        f.dispatcher.record(OwnedServiceRecord::Request {
            request_id: 5,
            op,
            args: vec![OwnedValue::U32(20)],
        });
    }
    f.receiver.try_recv().unwrap()
}
fn answer(
    f: &Fixture,
    command: shared::protocol::audio_cmd::AudioCmd,
    n: usize,
) -> Arc<AtomicBool> {
    use shared::protocol::audio_cmd::AudioCmd;
    let dropped = Arc::new(AtomicBool::new(false));
    match command {
        AudioCmd::GetAnalyserByteTimeDomainData { resp, .. }
        | AudioCmd::GetAnalyserByteFrequencyData { resp, .. } => {
            drop(resp.send(Ok(native(f, byte_values(n), dropped.clone()))));
        }
        AudioCmd::GetAnalyserFloatTimeDomainData { resp, .. }
        | AudioCmd::GetAnalyserFloatFrequencyData { resp, .. } => {
            drop(resp.send(Ok(native(f, float_values(n), dropped.clone()))));
        }
        _ => panic!("wrong analyser command"),
    }
    dropped
}
fn golden(kind: usize, n: usize) -> Vec<u8> {
    let bytes = if kind < 2 {
        byte_values(n)
    } else {
        float_values(n)
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect()
    };
    encode_down_record(&ServiceDownRecord::Reply {
        request_id: 5,
        outcome: Ok(OwnedValue::Bytes(bytes)),
    })
}
#[test]
fn all_analyser_wire_shapes_keep_final_credit_and_original_records() {
    for (kind, op) in OPS.into_iter().enumerate() {
        for n in if kind < 2 {
            vec![32, 32768]
        } else {
            vec![32, 16378, 16379, 32768]
        } {
            let bytes = n * if kind < 2 { 1 } else { 4 };
            let cap = 40 + bytes;
            let f = Fixture::new(cap + bytes);
            let command = request(&f, op);
            let native = answer(&f, command, n);
            f.settle();
            assert!(native.load(Ordering::Acquire));
            assert_eq!(
                f.ledger.used_bytes(),
                cap,
                "analyser wire lost final physical credit"
            );
            let first = f.host.outbox.take_message().unwrap();
            let record = if 24 + bytes <= MAX_INLINE_REPLY_BYTES {
                assert_eq!(
                    &first[16..],
                    golden(kind, n),
                    "inline analyser wire bytes changed"
                );
                first
            } else {
                let rows = read_down_message(&first).unwrap().1;
                let [
                    ServiceDownRecord::ReplyParked {
                        request_id,
                        byte_length,
                        ..
                    },
                ] = &rows[..]
                else {
                    panic!("analyser reply not parked")
                };
                assert_eq!(*byte_length as usize, 24 + bytes);
                assert!(f.host.outbox.take_parked(2, *request_id).is_none());
                let record = f.host.outbox.take_parked(1, *request_id).unwrap();
                assert_eq!(record.as_ref(), golden(kind, n));
                assert!(f.host.outbox.take_parked(1, *request_id).is_none());
                record
            };
            let ledger = f.ledger.clone();
            drop(f);
            assert_eq!(
                ledger.used_bytes(),
                cap,
                "host drop returned live analyser wire credit"
            );
            assert!(ledger.try_reserve(bytes + 1, "peer").is_err());
            drop(record);
            assert_eq!(ledger.used_bytes(), 0);
        }
    }
}
#[test]
fn analyser_wire_refusal_releases_native_output_and_answers_once() {
    for (kind, op) in OPS.into_iter().enumerate() {
        let n = 32768;
        let bytes = n * if kind < 2 { 1 } else { 4 };
        let f = Fixture::new(40 + 2 * bytes - 1);
        let command = request(&f, op);
        let native = answer(&f, command, n);
        f.settle();
        assert!(native.load(Ordering::Acquire));
        assert_eq!(
            f.ledger.used_bytes(),
            0,
            "analyser refusal leaked native or wire capacity"
        );
        let message = f.host.outbox.take_message().unwrap();
        assert!(matches!(
            &read_down_message(&message).unwrap().1[..],
            [ServiceDownRecord::Reply {
                request_id: 5,
                outcome: Err(_)
            }]
        ));
        assert!(f.host.outbox.take_message().is_none());
    }
}
#[test]
fn cancelled_analyser_reads_drop_unpolled_and_ready_outputs_once() {
    for op in OPS {
        for ready in [false, true] {
            let f = Fixture::new(1 << 20);
            let command = request(&f, op);
            let native = if ready {
                answer(&f, command, 32768)
            } else {
                f.cancel(5);
                answer(&f, command, 32768)
            };
            if ready {
                f.cancel(5);
            }
            f.runtime.block_on(async {
                tokio::task::yield_now().await;
            });
            assert!(
                native.load(Ordering::Acquire),
                "cancelled analyser kept native output"
            );
            assert_eq!(f.ledger.used_bytes(), 0);
            let message = f.host.outbox.take_message().unwrap();
            assert!(matches!(
                &read_down_message(&message).unwrap().1[..],
                [ServiceDownRecord::Reply {
                    request_id: 5,
                    outcome: Err(_)
                }]
            ));
            assert!(f.host.outbox.take_message().is_none());
        }
    }
}
