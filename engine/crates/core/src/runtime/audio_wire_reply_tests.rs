//! Exercise the actual ServiceHost, sync mailbox and owned response route.
use super::super::service_ops::id;
use super::*;
use frame_wire::{
    service::OUTCOME_OK,
    value::{OwnedValue, ValueWriter},
};
use shared::audio_physical::AudioAggregateLedger;
use shared::audio_resources::{AudioBufferFormat, AudioResourceRegistry};
use std::path::PathBuf;
const PCM: usize = 24;
const REPLY: usize = PCM + 12;
struct Fixture {
    path: SyncPath,
    registry: AudioResourceRegistry,
    physical: Arc<AudioAggregateLedger>,
    _receiver: shared::audio_channel::AudioCommandReceiver,
    turns: Option<std::thread::JoinHandle<()>>,
}
impl Fixture {
    fn new(max: usize) -> Self {
        let physical = Arc::new(AudioAggregateLedger::new(max));
        let registry = AudioResourceRegistry::with_physical_ledger(physical.clone());
        let (tx, receiver) = shared::audio_channel::channel();
        let sender = shared::op_state::AudioSender::hosted(
            tx,
            shared::channel::ThreadWakeup::new(),
            shared::op_state::AudioHostStartSignal::new(),
            registry.clone(),
        );
        let (host, mut work) = ServiceHost::new(
            1,
            PathBuf::new(),
            PathBuf::new(),
            Arc::new(WakerSlot::default()),
            Arc::default(),
        );
        host.context
            .bind_audio(sender, 7, shared::network_policy::NetworkPolicy::default());
        let turns = std::thread::spawn(move || {
            while let Some(work) = work.blocking_recv() {
                if let super::super::external_services::ServiceWork::Sync { turn } = work {
                    let _ = turn.send(());
                }
            }
        });
        let path = SyncPath::new(
            1,
            Arc::new(OnceLock::new()),
            Admission::new(Arc::new(Mutex::new(FrameIngress::new(0, 1)))),
            Arc::default(),
        )
        .with_services(host);
        Self {
            path,
            registry,
            physical,
            _receiver: receiver,
            turns: Some(turns),
        }
    }
    fn reserve(&self) -> shared::audio_resources::AudioBufferKey {
        self.registry
            .reserve_initial_backing(
                7,
                AudioBufferFormat {
                    channels: 2,
                    frames: 3,
                    sample_rate: 48000,
                },
            )
            .unwrap()
            .key()
    }
    fn params(key: shared::audio_resources::AudioBufferKey) -> Vec<u8> {
        let mut writer = ValueWriter::over(id::op_audio_materialize_buffer.to_le_bytes().to_vec());
        OwnedValue::U32(key.serial).write_to(&mut writer);
        writer.into_bytes()
    }
    fn call(&self, params: &[u8], limit: u32) -> AnsweredCall {
        let mut body = Vec::new();
        for word in [1u64, 1, 1, 0] {
            body.extend_from_slice(&word.to_le_bytes());
        }
        for word in [frame_wire::sync::SYNC_OP_SERVICE, limit, 10000, 0] {
            body.extend_from_slice(&word.to_le_bytes());
        }
        body.extend_from_slice(&0u64.to_le_bytes());
        body.extend_from_slice(params);
        self.path.answer(&body, 1)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.path.services.take();
        self.turns.take().unwrap().join().unwrap();
    }
}
#[test]
fn actual_owned_audio_reply_outlives_mailbox_host_and_registry() {
    let f = Fixture::new(PCM + REPLY);
    let key = f.reserve();
    let answer = f.call(&Fixture::params(key), REPLY as u32);
    assert_eq!(answer.answer.state, SyncState::Ready);
    assert_eq!(answer.reply.len(), REPLY);
    assert_eq!(
        u32::from_le_bytes(answer.reply[..4].try_into().unwrap()),
        OUTCOME_OK
    );
    assert_eq!(
        u32::from_le_bytes(answer.reply[4..8].try_into().unwrap()),
        frame_wire::value::TAG_BYTES
    );
    assert_eq!(
        u32::from_le_bytes(answer.reply[8..12].try_into().unwrap()),
        PCM as u32
    );
    assert!(
        answer.reply[12..].iter().all(|b| *b == 0),
        "actual sync route changed initial PCM"
    );
    assert_eq!(f.path.mailbox.lock().state(), SyncState::Free);
    f.registry.release_buffer(key);
    let physical = f.physical.clone();
    let pointer = answer.reply.as_ptr();
    drop(f);
    assert_eq!(
        physical.used_bytes(),
        REPLY,
        "actual owned sync route returned live reply credit"
    );
    let reply = answer.reply;
    assert_eq!(reply.as_ptr(), pointer);
    drop(reply);
    assert_eq!(
        physical.used_bytes(),
        0,
        "actual owned sync release leaked credit"
    );
}
#[test]
fn actual_audio_reply_refusals_preserve_initial_and_frozen_entries() {
    for initial in [true, false] {
        let f = Fixture::new(PCM * 2 + REPLY);
        let key = if initial {
            f.reserve()
        } else {
            f.registry
                .adopt_frozen(
                    7,
                    AudioBufferFormat {
                        channels: 2,
                        frames: 3,
                        sample_rate: 48000,
                    },
                    vec![2.0; 6],
                )
                .unwrap()
                .key()
        };
        let params = Fixture::params(key);
        let too_small = f.call(&params, (REPLY - 1) as u32);
        assert_eq!(too_small.answer.error, Some(SyncError::ReplyTooLarge));
        let held = if initial { 0 } else { PCM };
        assert_eq!(
            f.physical.used_bytes(),
            held,
            "reply limit refusal committed or leaked temporary PCM"
        );
        let peer = f.physical.try_reserve(PCM + 1, "peer").unwrap();
        let refused = f.call(&params, 4096);
        assert_eq!(refused.answer.state, SyncState::Ready);
        assert_eq!(
            u32::from_le_bytes(refused.reply[..4].try_into().unwrap()),
            frame_wire::service::OUTCOME_ERROR
        );
        drop(refused);
        drop(peer);
        assert_eq!(f.physical.used_bytes(), held);
        let retry = f.call(&params, REPLY as u32);
        assert_eq!(retry.answer.state, SyncState::Ready);
        assert_eq!(
            retry.reply.len(),
            REPLY,
            "sync refusal consumed original writable transition"
        );
        let pcm = if initial { 0f32 } else { 2f32 }.to_le_bytes();
        assert!(
            retry.reply[12..].chunks_exact(4).all(|v| v == pcm),
            "sync retry changed original PCM"
        );
        f.registry.release_buffer(key);
        drop(retry);
        assert_eq!(f.physical.used_bytes(), 0);
    }
}
#[test]
fn actual_shared_mailbox_keeps_credit_until_the_stored_bytes_are_replaced() {
    let f = Fixture::new(PCM + REPLY);
    let key = f.reserve();
    let params = Fixture::params(key);
    let request = SyncRequest {
        request_id: 0,
        runtime_generation: 1,
        surface_generation: 1,
        resource_epoch: 1,
        triggering_sequence: 0,
        operation: frame_wire::sync::SYNC_OP_SERVICE,
        max_reply_bytes: REPLY as u32,
        deadline_nanos: 10_000_000_000,
    };
    assert_eq!(
        f.path.post(request, &params, 1).unwrap().state,
        SyncState::Ready
    );
    assert_eq!(
        f.path.take_reply(&mut [0; REPLY - 1]),
        Err(SyncError::ReplyTooLarge)
    );
    let mut out = [0; REPLY];
    assert_eq!(f.path.take_reply(&mut out), Ok(REPLY));
    assert_eq!(
        f.physical.used_bytes(),
        REPLY,
        "mailbox acknowledgement returned stored wire credit"
    );
    f.registry.release_buffer(key);
    let next = f
        .path
        .post(
            SyncRequest {
                max_reply_bytes: 4096,
                ..request
            },
            &Fixture::params(key),
            1,
        )
        .unwrap();
    assert_eq!(next.state, SyncState::Ready);
    assert_eq!(
        f.physical.used_bytes(),
        0,
        "mailbox replacement leaked old wire credit"
    );
}
#[test]
fn late_audio_reply_frees_credit_without_settling_a_new_request() {
    let f = Fixture::new(PCM + REPLY);
    let key = f.reserve();
    let reply = f
        .path
        .service_call(&Fixture::params(key), REPLY as u32, 0, 10_000_000_000, 1)
        .unwrap();
    let mut mailbox = SyncMailbox::new(1);
    let newer = mailbox
        .post(
            SyncRequest {
                request_id: 0,
                runtime_generation: 1,
                surface_generation: 1,
                resource_epoch: 1,
                triggering_sequence: 0,
                operation: frame_wire::sync::SYNC_OP_SERVICE,
                max_reply_bytes: 4096,
                deadline_nanos: 10_000_000_000,
            },
            1,
        )
        .unwrap();
    assert!(settle(&mut mailbox, newer + 1, Ok(reply)).is_none());
    assert_eq!(mailbox.state(), SyncState::Pending);
    assert_eq!(mailbox.request().unwrap().request_id, newer);
    assert_eq!(
        f.physical.used_bytes(),
        0,
        "late wire response retained credit"
    );
    f.registry.release_buffer(key);
}
