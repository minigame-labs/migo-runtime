//! Observe dispatch through real typed Audio tasks, not just a priority field.
use super::{
    tests::{WAIT, blocked_worker, queue},
    *,
};
use crate::{scheduler::IoScheduler, task::PoolKind};
use std::sync::mpsc;

#[tokio::test(flavor = "current_thread")]
async fn refill_overtakes_an_older_background_decode_on_the_same_audio_lane() {
    let owner = IoScheduler::local_for_test(701, 1);
    let release = blocked_worker(&owner);
    let (sent, observed) = mpsc::channel();
    let background = owner.background_audio_cpu();
    let foreground = owner.audio_cpu();
    let decoded = sent.clone();
    let mut decode = Box::pin(background.run(move || decoded.send("decode").unwrap()));
    let mut refill = Box::pin(foreground.run(move || sent.send("refill").unwrap()));
    queue(decode.as_mut()).await;
    queue(refill.as_mut()).await;
    drop(release);
    assert_eq!(observed.recv_timeout(WAIT).unwrap(), "refill");
    assert_eq!(observed.recv_timeout(WAIT).unwrap(), "decode");
    decode.await.unwrap();
    refill.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn background_audio_keeps_the_existing_host_round_robin() {
    let (first, second) = IoScheduler::shared_executor_pair_for_test(702, 703, 1);
    let release = blocked_worker(&first);
    let (sent, observed) = mpsc::channel();
    let a = first.background_audio_cpu();
    let b = second.background_audio_cpu();
    let mut jobs = Vec::new();
    for (cpu, marker) in [(a.clone(), 1), (a, 1), (b, 2)] {
        let sent = sent.clone();
        let mut job = Box::pin(async move { cpu.run(move || sent.send(marker).unwrap()).await });
        queue(job.as_mut()).await;
        jobs.push(job);
    }
    drop(release);
    for marker in [1, 2, 1] {
        assert_eq!(observed.recv_timeout(WAIT).unwrap(), marker);
    }
    for job in jobs {
        job.await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn background_audio_ages_amid_a_sustained_foreground_io_backlog() {
    let owner = IoScheduler::local_for_test(704, 1);
    let release = blocked_worker(&owner);
    let (sent, observed) = mpsc::channel();
    let cpu = owner.background_audio_cpu();
    let background = sent.clone();
    let mut decode = Box::pin(cpu.run(move || background.send(100).unwrap()));
    queue(decode.as_mut()).await;
    let mut foreground = Vec::new();
    for marker in 0..40 {
        let sent = sent.clone();
        foreground.push(
            owner
                .pools()
                .submit_async(PoolKind::Fs, PriorityClass::ForegroundAsync, move || {
                    sent.send(marker).unwrap();
                })
                .unwrap(),
        );
    }
    drop(release);
    let order: Vec<_> = (0..41)
        .map(|_| observed.recv_timeout(WAIT).unwrap())
        .collect();
    // The blocked prelude was dispatch one. Fifteen foreground jobs follow,
    // then the existing sixteenth-dispatch aging opportunity admits Audio.
    assert_eq!(order.iter().position(|marker| *marker == 100), Some(15));
    assert_eq!(
        order
            .into_iter()
            .filter(|marker| *marker != 100)
            .collect::<Vec<_>>(),
        (0..40).collect::<Vec<_>>()
    );
    decode.await.unwrap();
    for result in foreground {
        result.await.unwrap().unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn foreground_and_background_share_admission_and_release_rejected_captures() {
    let owner = IoScheduler::local_for_test(705, 1);
    let _release = blocked_worker(&owner);
    let background = owner.background_audio_cpu();
    let foreground = owner.audio_cpu();
    let mut decode = Box::pin(background.run(|| ()));
    let mut refill = Box::pin(foreground.run(|| ()));
    queue(decode.as_mut()).await;
    queue(refill.as_mut()).await;
    for cpu in [background.clone(), foreground.clone()] {
        let input = Arc::new([0_u8; 8192]);
        let weak = Arc::downgrade(&input);
        let mut rejected = Box::pin(cpu.run(move || drop(input)));
        poll_fn(|cx| match rejected.as_mut().poll(cx) {
            Poll::Ready(Err(error)) => {
                assert_eq!(error.code, ErrorCode::InputSaturated);
                Poll::Ready(())
            }
            _ => panic!("both Audio priorities must share the full host quota"),
        })
        .await;
        drop(rejected);
        assert!(
            weak.upgrade().is_none(),
            "rejected input must retire immediately"
        );
    }
    drop(decode);
    drop(refill);
    assert_eq!(owner.pending_work_for_test(), 2);
}
