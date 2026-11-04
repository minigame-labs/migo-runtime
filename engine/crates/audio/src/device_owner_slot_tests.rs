use super::*;
use std::{
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

fn format(source: u32, target: u32, channels: u32) -> Format {
    Format {
        source,
        target,
        channels,
    }
}
fn controller(s: &migo_io::scheduler::IoScheduler, max: usize) -> Controller {
    Controller {
        host: Host {
            cpu: s.audio_cpu().background(),
            usage: Usage::new(max),
            process: Usage::new(8),
            aggregate: Arc::new(AudioAggregateLedger::new(8 * OWNER_BYTES)),
            retire_hook: None,
        },
        slots: Slots::new(),
    }
}
fn until(mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < end, "managed route failed to complete");
        thread::yield_now();
    }
}
fn empty(host: &Host) {
    until(|| host.usage.owners.load(Ordering::Acquire) == 0 && host.aggregate.used_bytes() == 0);
}
fn ensure(c: &mut Controller, ctx: &AudioContext, target: u32) {
    c.ensure(ctx, target, ThreadWakeup::new(), None).unwrap();
}
fn take(c: &mut Controller, ctx: &AudioContext) -> Device {
    let mut d = None;
    until(|| {
        d = c.take(ctx);
        d.is_some()
    });
    d.unwrap().unwrap()
}
fn make(host: &Host, id: u32, f: Format) -> Device {
    crate::streaming::get_stream_runtime()
        .block_on(host.prepare(
            id,
            f,
            Limits {
                source_ahead: MAX_AHEAD,
                pending_events: 64,
            },
            None,
        ))
        .unwrap()
}
fn bound(mut d: Device, slot: &Slot) -> Completion {
    d.binding = Some(Binding {
        epoch: slot.epoch.clone(),
        operation: slot.operation.clone(),
    });
    Completion { device: d }
}
struct Silence;
impl Graph for Silence {
    fn active(&self) -> bool {
        false
    }
}
impl Source for Silence {
    fn render(&mut self, out: &mut [f32]) -> EngineResult<Block> {
        Ok(Block {
            frames: out.len() / 2,
            eof: false,
        })
    }
    fn drain_ended(&mut self) -> impl Iterator<Item = u32> {
        std::iter::empty()
    }
}
struct Release(Option<mpsc::Sender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.take();
    }
}

#[test]
fn controller_uses_one_host_authority_and_ensure_does_not_restart_preparation() {
    let scheduler = migo_io::scheduler::IoScheduler::new(0);
    let mut c = controller(&scheduler, 1);
    let a = AudioContext::new(1, 44100, 2);
    let b = AudioContext::new(2, 44100, 2);
    ensure(&mut c, &a, 48000);
    let operation = c.slots[&1].operation.clone();
    ensure(&mut c, &a, 48000);
    assert!(
        Arc::ptr_eq(&operation, &c.slots[&1].operation),
        "idempotent ensure replaced its request"
    );
    let d = take(&mut c, &a);
    ensure(&mut c, &b, 48000);
    assert!(
        Arc::ptr_eq(&c.host.usage, &c.slots[&1].host.usage)
            && Arc::ptr_eq(&c.host.usage, &c.slots[&2].host.usage),
        "controller duplicated host authority per route"
    );
    let mut result = None;
    until(|| {
        result = c.take(&b);
        result.is_some()
    });
    assert_eq!(
        result.unwrap().err().map(|e| e.code),
        Some(ErrorCode::InputSaturated),
        "routes bypassed their common host quota"
    );
    drop(d);
    c.restart();
    empty(&c.host);
    let canonical = Controller::new(scheduler.audio_cpu());
    assert!(canonical.slots.is_empty());
}
#[test]
fn same_format_replacement_and_aba_cannot_accept_old_request_results() {
    let scheduler = migo_io::scheduler::IoScheduler::new(0);
    let h = controller(&scheduler, 8).host;
    let mut ctx = AudioContext::new(7, 44100, 2);
    let f = format(44100, 48000, 2);
    let mut slot = Slot::new(h.clone(), 7, ctx.device_epoch(), f);
    let old = bound(make(&h, 7, f), &slot);
    slot.request(ThreadWakeup::new(), None);
    assert!(
        !slot.accept(old),
        "same-format new request accepted a stale completion"
    );
    slot.invalidate();
    let stale = bound(make(&h, 7, f), &slot);
    ctx.renegotiate_device(96000, 2);
    ctx.renegotiate_device(48000, 2);
    let mut next = Slot::new(h.clone(), 7, ctx.device_epoch(), f);
    assert!(!next.accept(stale), "A-B-A route accepted an old owner");
    drop((slot, next));
    empty(&h);
}
#[test]
fn numeric_context_reuse_and_late_remove_do_not_revoke_the_new_owner() {
    let scheduler = migo_io::scheduler::IoScheduler::new(0);
    let mut c = controller(&scheduler, 8);
    let mut old = AudioContext::new(7, 44100, 2);
    ensure(&mut c, &old, 48000);
    let mut d = take(&mut c, &old);
    old.close();
    let current = AudioContext::new(7, 44100, 2);
    ensure(&mut c, &current, 48000);
    let current_request = c.slots[&7].operation.clone();
    c.remove(&old);
    assert!(
        c.slots.contains_key(&7) && Arc::ptr_eq(&current_request, &c.slots[&7].operation),
        "late old-context cleanup removed the new owner"
    );
    let mut out = [0.25; 256];
    assert_eq!(
        d.step(&mut Silence, &mut out).err().map(|e| e.code),
        Some(ErrorCode::Cancelled),
        "reused context ID kept its old device owner live"
    );
    assert_eq!(out, [0.25; 256]);
    drop(take(&mut c, &current));
    drop(d);
    c.restart();
    empty(&c.host);
}
#[test]
fn acceptance_rejects_foreign_authorities_missing_binding_and_wrong_metadata() {
    let scheduler = migo_io::scheduler::IoScheduler::new(0);
    let h = controller(&scheduler, 8).host;
    let foreign = controller(&scheduler, 8).host;
    let ctx = AudioContext::new(7, 44100, 2);
    let f = format(44100, 48000, 2);
    let mut slot = Slot::new(h.clone(), 7, ctx.device_epoch(), f);
    assert!(
        !slot.accept(Completion {
            device: make(&h, 7, f)
        }),
        "unbound owner was accepted"
    );
    assert!(
        !slot.accept(bound(make(&foreign, 7, f), &slot)),
        "foreign host owner was accepted"
    );
    assert!(
        !slot.accept(bound(make(&h, 8, f), &slot)),
        "wrong context owner was accepted"
    );
    assert!(
        !slot.accept(bound(make(&h, 7, format(44100, 96000, 2)), &slot)),
        "wrong format owner was accepted"
    );
    let mut cancelled_owner = make(&h, 7, f);
    cancelled_owner
        .session()
        .unwrap()
        .pull
        .ready
        .request
        .cancel
        .store(true, Ordering::Release);
    assert!(
        !slot.accept(bound(cancelled_owner, &slot)),
        "cancelled prepared owner was accepted"
    );
    let own = bound(make(&h, 7, f), &slot);
    assert!(slot.accept(own));
    drop(slot.take().unwrap().unwrap());
    drop(slot);
    empty(&h);
    empty(&foreign);
}
#[test]
fn closing_context_aborts_running_preparation_but_keeps_actual_cpu_credit() {
    let scheduler = migo_io::scheduler::IoScheduler::new(0);
    let mut c = controller(&scheduler, 8);
    let mut ctx = AudioContext::new(7, 768000, 2);
    let (tx, started) = mpsc::channel();
    let (unlock, wait) = mpsc::channel::<()>();
    let release = Release(Some(unlock));
    let wait = Mutex::new(Some(wait));
    let hook: Hook = Arc::new(move |ready| {
        if ready.stream.as_ref().unwrap().prepared_layers() == 1
            && let Some(wait) = wait.lock().unwrap().take()
        {
            tx.send(()).unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(10));
        }
    });
    c.ensure(&ctx, 3000, ThreadWakeup::new(), Some(hook))
        .unwrap();
    started.recv_timeout(Duration::from_secs(10)).unwrap();
    ctx.close();
    assert_eq!(
        c.take(&ctx).unwrap().err().map(|e| e.code),
        Some(ErrorCode::Cancelled),
        "closed context accepted its preparing owner"
    );
    assert_eq!(
        c.host.aggregate.used_bytes(),
        OWNER_BYTES,
        "context close returned credit before the running owner died"
    );
    drop(release);
    c.restart();
    empty(&c.host);
}
#[test]
fn native_bypass_is_lazy_and_closed_hosts_reject_cached_completion() {
    let scheduler = migo_io::scheduler::IoScheduler::new(0);
    let mut c = controller(&scheduler, 8);
    let mut ctx = AudioContext::new(7, 48000, 2);
    ensure(&mut c, &ctx, 48000);
    assert!(
        c.slots[&7].pending.is_none() && c.take(&ctx).is_none(),
        "native bypass constructed a device filter"
    );
    assert_eq!(c.host.aggregate.used_bytes(), 0);
    ctx.renegotiate_device(44100, 2);
    ensure(&mut c, &ctx, 44100);
    until(|| {
        c.slots.get_mut(&7).unwrap().poll();
        c.slots[&7].result.is_some()
    });
    scheduler.close();
    assert_eq!(
        c.take(&ctx).unwrap().err().map(|e| e.code),
        Some(EngineError::from(migo_io::pools::PoolError::Closed).code),
        "cached owner escaped host close"
    );
    c.restart();
    empty(&c.host);
}
#[test]
fn restart_revokes_escaped_devices_and_keeps_other_hosts_live() {
    let a = migo_io::scheduler::IoScheduler::new(0);
    let b = migo_io::scheduler::IoScheduler::new(0);
    let mut first = controller(&a, 8);
    let mut second = controller(&b, 8);
    let ctx = AudioContext::new(1, 44100, 2);
    let other = AudioContext::new(1, 44100, 2);
    ensure(&mut first, &ctx, 48000);
    let mut old = take(&mut first, &ctx);
    ensure(&mut second, &other, 48000);
    let mut kept = take(&mut second, &other);
    first.restart();
    let mut out = [0.0; 256];
    assert_eq!(
        old.step(&mut Silence, &mut out).err().map(|e| e.code),
        Some(ErrorCode::Cancelled),
        "restart left an escaped owner live"
    );
    assert!(
        kept.step(&mut Silence, &mut out).is_ok(),
        "restart revoked another host"
    );
    drop((old, kept));
    second.restart();
    empty(&first.host);
    empty(&second.host);
}
#[test]
fn revoked_binding_blocks_cached_pcm_and_ended_without_heavy_render_destruction() {
    for event_first in [false, true] {
        let scheduler = migo_io::scheduler::IoScheduler::new(0);
        let mut c = controller(&scheduler, 8);
        let mut ctx = AudioContext::new(7, 44100, 2);
        ensure(&mut c, &ctx, 48000);
        let mut d = take(&mut c, &ctx);
        let session = d.session().unwrap();
        session.pull.ready.output.extend_from_slice(&[0.8; 256]);
        session
            .pull
            .buffers
            .as_mut()
            .unwrap()
            .events
            .push_back(Event { id: 9, fence: 0 });
        ctx.renegotiate_device(48000, 2);
        let mut out = [0.25; 256];
        let before = migo_alloc_probe::thread_counts();
        let code = if event_first {
            d.pop_ended().err().map(|e| e.code)
        } else {
            d.step(&mut Silence, &mut out).err().map(|e| e.code)
        };
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(
            code,
            Some(ErrorCode::Cancelled),
            "revoked route published cached PCM or ended"
        );
        assert_eq!(out, [0.25; 256]);
        assert!(
            after.bytes_freed - before.bytes_freed < 1024,
            "route revocation destroyed filters on the render caller"
        );
        assert!(d.lease.is_none());
        drop(d);
        c.restart();
        empty(&c.host);
    }
}
#[test]
fn cached_take_revalidates_epoch_and_suspension_keeps_the_same_request() {
    let scheduler = migo_io::scheduler::IoScheduler::new(0);
    let mut c = controller(&scheduler, 8);
    let mut ctx = AudioContext::new(7, 44100, 2);
    ensure(&mut c, &ctx, 48000);
    until(|| {
        c.slots.get_mut(&7).unwrap().poll();
        c.slots[&7].result.is_some()
    });
    let request = c.slots[&7].operation.clone();
    ctx.suspend();
    ctx.resume();
    ensure(&mut c, &ctx, 48000);
    assert!(
        Arc::ptr_eq(&request, &c.slots[&7].operation),
        "suspension rebuilt its prepared request"
    );
    ctx.renegotiate_device(48000, 2);
    assert_eq!(
        c.take(&ctx).unwrap().err().map(|e| e.code),
        Some(ErrorCode::Cancelled),
        "route changed after completion but stale take succeeded"
    );
    assert!(
        c.slots[&7].result.is_none(),
        "stale take retained a cached owner"
    );
    empty(&c.host);
    c.restart();
    empty(&c.host);
}
#[test]
fn active_bound_device_retains_reference_pcm_and_its_first_terminal_code() {
    struct Pulse {
        left: usize,
        record: Vec<f32>,
        bad: bool,
    }
    impl Graph for Pulse {
        fn active(&self) -> bool {
            self.left > 0 || self.bad
        }
    }
    impl Source for Pulse {
        fn render(&mut self, out: &mut [f32]) -> EngineResult<Block> {
            if self.bad {
                return Err(EngineError::from_detail(
                    ErrorCode::NotFound,
                    "bound source error",
                ));
            }
            let n = self.left.min(out.len() / 2);
            for (i, v) in out[..n * 2].iter_mut().enumerate() {
                *v = ((i + self.left * 17) % 251) as f32 / 128.0 - 0.9;
            }
            self.left -= n;
            self.record.extend_from_slice(out);
            Ok(Block {
                frames: out.len() / 2,
                eof: false,
            })
        }
        fn drain_ended(&mut self) -> impl Iterator<Item = u32> {
            std::iter::empty()
        }
    }
    let scheduler = migo_io::scheduler::IoScheduler::new(0);
    let mut c = controller(&scheduler, 8);
    let mut ctx = AudioContext::new(7, 44100, 2);
    ensure(&mut c, &ctx, 48000);
    let mut d = take(&mut c, &ctx);
    let mut graph = Pulse {
        left: 513,
        record: Vec::new(),
        bad: false,
    };
    let mut out = [0.0; 254];
    let mut pcm = Vec::new();
    let mut done = false;
    for _ in 0..100000 {
        out.fill(0.0);
        let turn = d.step(&mut graph, &mut out).unwrap();
        if let SessionTurn::Work(Turn::Published(n)) = turn {
            pcm.extend_from_slice(&out[..n * 2]);
        }
        if turn == SessionTurn::Quiescent {
            done = true;
            break;
        }
    }
    assert!(done);
    graph.record.resize(graph.record.len() + 4096 * 2, 0.0);
    let reference = crate::resample_job::tests::reference(
        &crate::decoder::DecodedAudio {
            sample_rate: 44100,
            channels: 2,
            samples: graph.record.clone(),
        },
        48000,
    );
    assert_eq!(
        pcm,
        reference[..pcm.len()],
        "binding changed complete mature PCM"
    );
    graph.bad = true;
    let mut code = None;
    for _ in 0..10000 {
        code = d.step(&mut graph, &mut out).err().map(|e| e.code);
        if code.is_some() {
            break;
        }
    }
    assert_eq!(
        code,
        Some(ErrorCode::NotFound),
        "bound source error was never observed"
    );
    ctx.renegotiate_device(48000, 2);
    assert_eq!(
        d.step(&mut graph, &mut out).err().map(|e| e.code),
        Some(ErrorCode::NotFound),
        "later route revocation replaced a terminal source error"
    );
    drop(d);
    c.restart();
    empty(&c.host);
}
