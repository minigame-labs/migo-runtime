use super::*;
use crate::scheduler::IoScheduler;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

struct Record {
    drops: Arc<Mutex<Vec<thread::ThreadId>>>,
    panic: bool,
}
impl Drop for Record {
    fn drop(&mut self) {
        self.drops.lock().push(thread::current().id());
        assert!(!self.panic, "controlled retirement destructor panic");
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn a_retirement_burst_preserves_live_foreground_admission() {
    let (live, closing) = IoScheduler::shared_executor_pair_for_test(0, 1, 1);
    let foreground = live.audio_cpu();
    let retired = closing.audio_cpu();
    let release = tests::blocked_worker(&live);
    let drops = Arc::new(Mutex::new(Vec::new()));
    closing.close();
    runtime().block_on(async {
        let mut cleanup = Vec::new();
        for cpu in [&foreground, &retired, &foreground, &retired] {
            cleanup.push(Box::pin(cpu.retirement().retire(Record {
                drops: drops.clone(),
                panic: false,
            })));
        }
        for future in &mut cleanup {
            tests::queue(future.as_mut()).await;
        }
        let mut request = pin!(foreground.run(|| 42));
        let state = poll_fn(|cx| Poll::Ready(request.as_mut().poll(cx))).await;
        // Release and drain all actual owners before any failing assertion.
        drop(release);
        for future in cleanup {
            future.await;
        }
        let result = match state {
            Poll::Ready(result) => result,
            Poll::Pending => request.await,
        };
        assert_eq!(
            result.expect("cleanup burst denied live foreground admission"),
            42
        );
        assert_eq!(drops.lock().len(), 4, "cleanup burst lost an actual owner");
    });
}

#[test]
fn held_retirement_does_not_block_an_independent_executor() {
    struct Held {
        entered: std::sync::mpsc::Sender<()>,
        wait: std::sync::mpsc::Receiver<()>,
    }
    impl Drop for Held {
        fn drop(&mut self) {
            let _ = self.entered.send(());
            let _ = self.wait.recv_timeout(tests::WAIT);
        }
    }
    let first = IoScheduler::local_for_test(0, 1);
    let second = IoScheduler::local_for_test(0, 1);
    let drops = Arc::new(Mutex::new(Vec::new()));
    let (entered, observed) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    runtime().block_on(async {
        let mut held = pin!(
            first
                .audio_cpu()
                .retirement()
                .retire(Held { entered, wait })
        );
        tests::queue(held.as_mut()).await;
        observed.recv_timeout(tests::WAIT).unwrap();
        let mut other = pin!(second.audio_cpu().retirement().retire(Record {
            drops: drops.clone(),
            panic: false,
        }));
        let independent =
            tokio::time::timeout(std::time::Duration::from_millis(200), other.as_mut())
                .await
                .is_ok();
        drop(release);
        held.await;
        if !independent {
            other.await;
        }
        assert!(
            independent,
            "cleanup on one executor blocked an independent executor"
        );
        assert_eq!(drops.lock().len(), 1);
    });
}

#[test]
fn running_retirement_keeps_a_cpu_slot_for_live_work() {
    struct Held {
        entered: std::sync::mpsc::Sender<()>,
        wait: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
    }
    impl Drop for Held {
        fn drop(&mut self) {
            let _ = self.entered.send(());
            let _ = self.wait.lock().recv_timeout(tests::WAIT);
        }
    }
    let host = IoScheduler::local_for_test(0, 3);
    let cpu = host.audio_cpu();
    let (entered, observed) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let wait = Arc::new(Mutex::new(wait));
    runtime().block_on(async {
        let mut first = pin!(cpu.retirement().retire(Held {
            entered: entered.clone(),
            wait: wait.clone()
        }));
        let mut second = pin!(cpu.retirement().retire(Held { entered, wait }));
        tests::queue(first.as_mut()).await;
        observed.recv_timeout(tests::WAIT).unwrap();
        tests::queue(second.as_mut()).await;
        let mut live = pin!(cpu.run(|| 42));
        let ready =
            tokio::time::timeout(std::time::Duration::from_millis(200), live.as_mut()).await;
        let progressed = matches!(&ready, Ok(Ok(42)));
        drop(release);
        first.await;
        second.await;
        if ready.is_err() {
            let _ = live.await;
        }
        assert!(progressed, "running cleanup occupied every audio CPU slot");
    });
}

#[test]
fn retirement_after_close_uses_the_original_worker_without_reviving_host_work() {
    let host = IoScheduler::local_for_test(0, 1);
    let cpu = host.audio_cpu();
    let retirement = cpu.retirement();
    let drops = Arc::new(Mutex::new(Vec::new()));
    runtime().block_on(async {
        let worker = cpu.run(|| thread::current().id()).await.unwrap();
        host.close();
        assert!(
            !retirement.cpu.is_closed(),
            "retirement inherited host closure"
        );
        assert!(
            cpu.run(|| 1).await.is_err(),
            "closed host accepted CPU work"
        );
        retirement
            .retire(Record {
                drops: drops.clone(),
                panic: false,
            })
            .await;
        assert_eq!(
            *drops.lock(),
            vec![worker],
            "retirement left the original CPU worker"
        );
        assert_eq!(host.pools().started_thread_count_for_test(), 1);
        assert!(!cpu.refill_pending());
        assert_eq!(
            cpu.refill_generation(),
            1,
            "retirement advertised foreground refill"
        );
        assert!(cpu.is_closed(), "retirement reopened the host domain");
    });
}

#[test]
fn saturated_retirement_keeps_the_value_and_retries_without_inline_destruction() {
    let host = IoScheduler::local_for_test(0, 1);
    let cpu = host.audio_cpu();
    let release = tests::blocked_worker(&host);
    let drops = Arc::new(Mutex::new(Vec::new()));
    runtime().block_on(async {
        let mut first = pin!(cpu.run(|| ()));
        let mut second = pin!(cpu.run(|| ()));
        tests::queue(first.as_mut()).await;
        tests::queue(second.as_mut()).await;
        assert_eq!(
            cpu.run(|| ()).await.unwrap_err().code,
            ErrorCode::InputSaturated
        );
        let mut retirement = pin!(cpu.retirement().retire(Record {
            drops: drops.clone(),
            panic: false
        }));
        let state = poll_fn(|cx| Poll::Ready(retirement.as_mut().poll(cx))).await;
        assert!(
            drops.lock().is_empty(),
            "rejected retirement destroyed the queued owner inline"
        );
        assert!(state.is_pending(), "retirement abandoned a saturated owner");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        tests::queue(retirement.as_mut()).await;
        assert!(
            drops.lock().is_empty(),
            "rejected retirement destroyed the queued owner inline"
        );
        assert_eq!(
            host.pending_work_for_test(),
            2,
            "retirement bypassed the host Audio queue cap"
        );
        host.close();
        drop(release);
        assert!(first.await.is_err());
        assert!(second.await.is_err());
        retirement.await;
        let worker = host
            .pools()
            .submit_async(crate::task::PoolKind::Fs, PriorityClass::Background, || {
                thread::current().id()
            })
            .unwrap()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            *drops.lock(),
            vec![worker],
            "saturated retirement did not reach the original CPU worker"
        );
    });
}

#[test]
fn retirement_keeps_the_executor_alive_after_scheduler_drop() {
    let host = IoScheduler::local_for_test(0, 1);
    let cpu = host.audio_cpu();
    let retirement = cpu.retirement();
    let worker = runtime()
        .block_on(cpu.run(|| thread::current().id()))
        .unwrap();
    let drops = Arc::new(Mutex::new(Vec::new()));
    drop(host);
    drop(cpu);
    runtime().block_on(retirement.retire(Record {
        drops: drops.clone(),
        panic: false,
    }));
    assert_eq!(
        *drops.lock(),
        vec![worker],
        "retirement lost the original executor lifetime"
    );
}

#[test]
fn a_panicking_destructor_is_not_retried_and_the_worker_remains_usable() {
    struct Credit(Arc<AtomicUsize>);
    impl Drop for Credit {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Release);
        }
    }
    let host = IoScheduler::local_for_test(0, 1);
    let cpu = host.audio_cpu();
    let drops = Arc::new(Mutex::new(Vec::new()));
    let credits = Arc::new(AtomicUsize::new(0));
    runtime().block_on(async {
        cpu.retirement()
            .retire((
                Record {
                    drops: drops.clone(),
                    panic: true,
                },
                Credit(credits.clone()),
            ))
            .await;
        assert_eq!(
            drops.lock().len(),
            1,
            "retirement retried an already consumed destructor"
        );
        assert_eq!(
            credits.load(Ordering::Acquire),
            1,
            "destructor unwind lost physical credit"
        );
        assert_eq!(cpu.run(|| 17).await.unwrap(), 17);
    });
}
