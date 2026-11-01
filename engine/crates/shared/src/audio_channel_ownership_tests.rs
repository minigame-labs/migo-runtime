//! Queue credit must survive dequeue, receiver close and the actual destructor.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(super) fn physical_lifetime_and_origin() {
    let bytes = 4 << 20;
    let (tx, rx) = channel_with_limits(1, bytes);
    let data = Arc::new(vec![0_u8; bytes]);
    let weak = Arc::downgrade(&data);
    let (resp, mut answer) = tokio::sync::oneshot::channel();
    tx.try_send(AudioCmd::DecodeAudioData {
        ctx_id: 1,
        data,
        resp,
    })
    .unwrap();
    let delivery = rx.try_recv_owned().unwrap();
    assert!(
        matches!(tx.try_reserve_data(0), Err(AudioCommandReserveError::Full)),
        "owned command returned credit at dequeue"
    );
    let discarded = delivery.into_discarded_payload().unwrap();
    assert!(
        matches!(
            answer.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ),
        "discard retained a response sender"
    );
    drop(rx);
    assert_eq!(
        (
            tx.usage.items.load(Ordering::Acquire),
            tx.usage.bytes.load(Ordering::Acquire)
        ),
        (1, bytes),
        "receiver close returned live command credit"
    );
    assert!(weak.upgrade().is_some());
    drop(discarded);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        (
            tx.usage.items.load(Ordering::Acquire),
            tx.usage.bytes.load(Ordering::Acquire)
        ),
        (0, 0)
    );

    struct BlockDrop {
        entered: std::sync::mpsc::Sender<()>,
        release: Arc<AtomicBool>,
    }
    impl Drop for BlockDrop {
        fn drop(&mut self) {
            self.entered.send(()).unwrap();
            while !self.release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
        }
    }
    let (tx, _rx) = channel_with_limits(1, bytes);
    let permit = tx.try_reserve_data(bytes).unwrap();
    let (entered, waiting) = std::sync::mpsc::channel();
    let release = Arc::new(AtomicBool::new(false));
    let value = AudioDiscardedPayload {
        _payload: Box::new(BlockDrop {
            entered,
            release: release.clone(),
        }),
        _permit: Some(permit),
    };
    let worker = std::thread::spawn(move || drop(value));
    let started = waiting
        .recv_timeout(std::time::Duration::from_secs(5))
        .is_ok();
    let held = tx.usage.bytes.load(Ordering::Acquire) == bytes
        && tx.usage.items.load(Ordering::Acquire) == 1;
    release.store(true, Ordering::Release);
    worker.join().unwrap();
    assert!(
        started && held,
        "discarded payload returned credit before actual destructor"
    );

    let (left, _left_rx) = channel_with_limits(1, 16);
    let (right, _right_rx) = channel_with_limits(1, 16);
    let permit = left.try_reserve_data(0).unwrap();
    let committed = AtomicBool::new(false);
    let result = right.try_send_reserved_committing(
        AudioCmd::CreateContext {
            ctx_id: 1,
            sample_rate: None,
        },
        permit,
        || {
            committed.store(true, Ordering::Release);
        },
    );
    assert!(
        matches!(result, Err(AudioCommandSendError::ByteLimit(_)))
            && !committed.load(Ordering::Acquire),
        "foreign reservation crossed transport"
    );
}
