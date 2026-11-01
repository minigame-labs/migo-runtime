//! Exercise the real dispatcher, queued/parked record and returned byte owner.
use super::*;
use frame_wire::service::read_down_message;
use shared::{
    audio_frequency_response::{FrequencyResponse, FrequencyResponseStorage},
    audio_physical::AudioAggregateLedger,
    audio_resources::AudioResourceRegistry,
};
use std::sync::atomic::{AtomicBool, Ordering};
struct Storage {
    data: (Vec<f32>, Vec<f32>),
    _physical: shared::audio_physical::AudioAggregatePermit,
    dropped: Arc<AtomicBool>,
}
impl FrequencyResponseStorage for Storage {
    fn values(&self) -> (&[f32], &[f32]) {
        assert!(!self.dropped.load(Ordering::Acquire));
        (&self.data.0, &self.data.1)
    }
}
impl Drop for Storage {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}
struct Fixture {
    host: Arc<ServiceHost>,
    dispatcher: ServiceDispatcher,
    receiver: shared::audio_channel::AudioCommandReceiver,
    runtime: tokio::runtime::Runtime,
    ledger: Arc<AudioAggregateLedger>,
}
impl Fixture {
    fn new(max: usize) -> Self {
        let ledger = Arc::new(AudioAggregateLedger::new(max));
        let (mut host, _) = ServiceHost::new(
            1,
            PathBuf::new(),
            PathBuf::new(),
            Arc::new(WakerSlot::default()),
            Arc::default(),
        );
        Arc::get_mut(&mut host).unwrap().outbox = Arc::new(ServiceOutbox::with_physical_ledger(
            1,
            Arc::new(WakerSlot::default()),
            ledger.clone(),
        ));
        let (tx, receiver) = shared::audio_channel::channel();
        let sender = shared::op_state::AudioSender::hosted(
            tx,
            shared::channel::ThreadWakeup::new(),
            shared::op_state::AudioHostStartSignal::new(),
            AudioResourceRegistry::with_physical_ledger(ledger.clone()),
        );
        host.context
            .bind_audio(sender, 7, shared::network_policy::NetworkPolicy::default());
        let (sender, _) = shared::render_command_sender::CommandSender::new();
        let render = RenderHandles {
            canvas: shared::op_state::CanvasOpState::for_host(sender, 1),
            gpu_caps: shared::device::gpu_caps::GpuCaps::new(),
        };
        let dispatcher = ServiceDispatcher::new(&host, render);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        Self {
            host,
            dispatcher,
            receiver,
            runtime,
            ledger,
        }
    }
    fn request(
        &self,
        id: u32,
        n: usize,
    ) -> shared::protocol::audio_cmd::AudioResp<FrequencyResponse> {
        let _enter = self.runtime.enter();
        let args = vec![OwnedValue::U32(20), OwnedValue::Bytes(vec![0; n * 4])];
        if std::env::var("MIGO_FREQUENCY_WIRE_FAULT").as_deref() == Ok("legacy") {
            // The actual original serializer/outbox path, used only for the fresh
            // counterfactual cohort; production dispatch uses the dedicated wire route.
            let future = self
                .host
                .context
                .call_async(
                    id::op_audio_get_frequency_response,
                    args,
                    &self.dispatcher.render,
                )
                .unwrap();
            let outbox = self.host.outbox.clone();
            let in_flight = self.dispatcher.in_flight.clone();
            let mut map = self.dispatcher.in_flight.lock();
            let task = tokio::spawn(async move {
                let result = future.await;
                if in_flight.lock().remove(&id).is_some() {
                    outbox.reply(id, result);
                }
            });
            map.insert(id, task.abort_handle());
        } else {
            self.dispatcher.record(OwnedServiceRecord::Request {
                request_id: id,
                op: id::op_audio_get_frequency_response,
                args,
            });
        }
        let shared::protocol::audio_cmd::AudioCmd::GetFrequencyResponse { resp, .. } =
            self.receiver.try_recv().unwrap()
        else {
            panic!("actual frequency command")
        };
        resp
    }
    fn respond(
        &self,
        resp: shared::protocol::audio_cmd::AudioResp<FrequencyResponse>,
        n: usize,
    ) -> Arc<AtomicBool> {
        let magnitude = (0..n)
            .map(|i| {
                f32::from_bits(if i == 0 {
                    0x80000000
                } else {
                    (i as f32 * 0.125).to_bits()
                })
            })
            .collect::<Vec<_>>();
        let phase = (0..n)
            .map(|i| {
                if i == 0 {
                    f32::from_bits(0x7fc00003)
                } else {
                    -(i as f32) * 0.25
                }
            })
            .collect::<Vec<_>>();
        let dropped = Arc::new(AtomicBool::new(false));
        let physical = self
            .ledger
            .try_reserve(n * 8, "native frequency arrays")
            .unwrap();
        let response = FrequencyResponse::new(Storage {
            data: (magnitude, phase),
            _physical: physical,
            dropped: dropped.clone(),
        });
        if let Err(value) = resp.send(Ok(response)) {
            drop(value);
        }
        dropped
    }
    fn settle(&self) {
        self.runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !self.dispatcher.in_flight.lock().is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        });
    }
    fn cancel(&self, id: u32) {
        let _enter = self.runtime.enter();
        self.dispatcher
            .record(OwnedServiceRecord::Cancel { request_id: id });
        self.runtime.block_on(async {
            tokio::task::yield_now().await;
        });
    }
}
fn want(id: u32, n: usize) -> Vec<u8> {
    let a = (0..n)
        .flat_map(|i| {
            f32::from_bits(if i == 0 {
                0x80000000
            } else {
                (i as f32 * 0.125).to_bits()
            })
            .to_le_bytes()
        })
        .collect();
    let b = (0..n)
        .flat_map(|i| {
            if i == 0 {
                f32::from_bits(0x7fc00003)
            } else {
                -(i as f32) * 0.25
            }
            .to_le_bytes()
        })
        .collect();
    encode_down_record(&ServiceDownRecord::Reply {
        request_id: id,
        outcome: Ok(OwnedValue::Array(vec![
            OwnedValue::Bytes(a),
            OwnedValue::Bytes(b),
        ])),
    })
}
#[test]
fn actual_inline_frequency_reply_keeps_credit_and_uses_reserved_header() {
    let n = 5;
    let capacity = 56 + n * 8;
    let f = Fixture::new(capacity + n * 8);
    let response = f.request(5, n);
    let native = f.respond(response, n);
    f.settle();
    assert!(
        native.load(Ordering::Acquire),
        "frequency encoder retained native arrays"
    );
    assert_eq!(
        f.ledger.used_bytes(),
        capacity,
        "frequency wire reply lost physical credit"
    );
    let pointer = f.host.outbox.inner.lock().records.front().unwrap().as_ptr();
    let before = migo_alloc_probe::thread_counts();
    let message = ServiceHandle(f.host.clone()).take_message().unwrap();
    assert_eq!(
        migo_alloc_probe::thread_counts().bytes_allocated,
        before.bytes_allocated,
        "inline frequency drain allocated a copy"
    );
    assert_eq!(
        message.as_ptr().wrapping_add(SERVICE_DOWN_HEADER_BYTES),
        pointer,
        "inline frequency envelope copied record storage"
    );
    assert_eq!(
        &message[SERVICE_DOWN_HEADER_BYTES..],
        want(5, n),
        "frequency wire bytes changed from original encoder"
    );
    let ledger = f.ledger.clone();
    drop(f);
    assert_eq!(
        ledger.used_bytes(),
        capacity,
        "host drop returned live frequency wire credit"
    );
    drop(message);
    assert_eq!(
        ledger.used_bytes(),
        0,
        "inline frequency final release leaked credit"
    );
}
#[test]
fn actual_parked_frequency_reply_moves_once_and_outlives_host() {
    for n in [8188, 16384] {
        let capacity = 56 + n * 8;
        let f = Fixture::new(capacity + n * 8);
        let response = f.request(9, n);
        f.respond(response, n);
        f.settle();
        let notice = f.host.outbox.take_message().unwrap();
        let (_, records) = read_down_message(&notice).unwrap();
        assert!(
            matches!(&records[..],[ServiceDownRecord::ReplyParked{request_id:9,byte_length}]if*byte_length==40+n as u32*8)
        );
        assert!(
            f.host.outbox.take_parked(2, 9).is_none(),
            "wrong generation consumed parked audio"
        );
        let pointer = f.host.outbox.inner.lock().parked.get(&9).unwrap().as_ptr();
        let record = ServiceHandle(f.host.clone()).take_parked(1, 9).unwrap();
        assert_eq!(
            record.as_ptr(),
            pointer,
            "parked frequency drain copied record"
        );
        assert_eq!(
            record.as_ref(),
            want(9, n),
            "parked frequency wire bytes changed"
        );
        assert!(
            f.host.outbox.take_parked(1, 9).is_none(),
            "parked audio was returned twice"
        );
        let ledger = f.ledger.clone();
        drop(notice);
        drop(f);
        assert_eq!(
            ledger.used_bytes(),
            capacity,
            "parked frequency owner lost credit at host drop"
        );
        let peer = ledger.try_reserve(n * 8, "peer").unwrap();
        assert!(ledger.try_reserve(1, "another peer").is_err());
        drop(peer);
        drop(record);
        assert_eq!(
            ledger.used_bytes(),
            0,
            "parked frequency final release leaked credit"
        );
    }
}
#[test]
fn actual_frequency_boundary_and_mixed_message_order_are_preserved() {
    for n in [0, 8187] {
        let capacity = 56 + n * 8;
        let f = Fixture::new(capacity + n * 8);
        f.host.outbox.event(11, vec![OwnedValue::U32(1)]);
        let response = f.request(5, n);
        f.respond(response, n);
        f.settle();
        f.host.outbox.event(12, vec![OwnedValue::U32(2)]);
        let first = f.host.outbox.take_message().unwrap();
        assert!(matches!(
            &read_down_message(&first).unwrap().1[..],
            [ServiceDownRecord::Event { event: 11, .. }]
        ));
        let audio = f.host.outbox.take_message().unwrap();
        assert_eq!(&audio[16..], want(5, n));
        let last = f.host.outbox.take_message().unwrap();
        assert!(matches!(
            &read_down_message(&last).unwrap().1[..],
            [ServiceDownRecord::Event { event: 12, .. }]
        ));
        assert!(f.host.outbox.take_message().is_none());
        assert_eq!(f.host.outbox.queued_bytes(), 0);
        drop(audio);
        assert_eq!(f.ledger.used_bytes(), 0);
    }
}
#[test]
fn actual_frequency_physical_refusal_returns_an_error_without_wire_storage() {
    let n = 16384;
    let capacity = 56 + n * 8;
    let f = Fixture::new(capacity + n * 8 - 1);
    let response = f.request(5, n);
    let native = f.respond(response, n);
    f.settle();
    assert!(native.load(Ordering::Acquire));
    assert_eq!(
        f.ledger.used_bytes(),
        0,
        "frequency wire refusal leaked physical credit"
    );
    let message = f.host.outbox.take_message().unwrap();
    assert!(
        matches!(
            &read_down_message(&message).unwrap().1[..],
            [ServiceDownRecord::Reply {
                request_id: 5,
                outcome: Err(_)
            }]
        ),
        "frequency wire refusal was not answered"
    );
}
#[test]
fn actual_outbox_refusal_drops_the_encoded_frequency_owner() {
    let f = Fixture::new(136);
    let response = f.request(5, 5);
    f.host.outbox.inner.lock().bytes = MAX_OUTBOX_BYTES;
    f.respond(response, 5);
    f.settle();
    f.runtime.block_on(async {
        tokio::task::yield_now().await;
    });
    assert_eq!(
        f.ledger.used_bytes(),
        0,
        "outbox refusal retained encoded frequency credit"
    );
    let message = f.host.outbox.take_message().unwrap();
    assert!(matches!(
        &read_down_message(&message).unwrap().1[..],
        [ServiceDownRecord::Reply {
            outcome: Err(_),
            ..
        }]
    ));
}
#[test]
fn actual_cancelled_frequency_response_retires_native_arrays_once() {
    for ready in [false, true] {
        let f = Fixture::new(136);
        let response = f.request(5, 5);
        let native = if ready {
            f.respond(response, 5)
        } else {
            f.cancel(5);
            f.respond(response, 5)
        };
        if ready {
            f.cancel(5);
        }
        f.runtime.block_on(async {
            tokio::task::yield_now().await;
        });
        f.runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !native.load(Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        });
        assert!(
            native.load(Ordering::Acquire),
            "cancelled frequency native owner survived"
        );
        assert_eq!(
            f.ledger.used_bytes(),
            0,
            "cancelled frequency response retained physical credit"
        );
        let message = f.host.outbox.take_message().unwrap();
        assert!(
            matches!(&read_down_message(&message).unwrap().1[..],[ServiceDownRecord::Reply{request_id:5,outcome:Err(ReplyError{class,..})}]if class=="AbortError")
        );
        assert!(
            f.host.outbox.take_message().is_none(),
            "cancel and completion answered twice"
        );
    }
}

#[path = "audio_analyser_wire_tests.rs"]
mod analyser;
