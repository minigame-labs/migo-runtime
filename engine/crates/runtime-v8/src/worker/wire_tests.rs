use super::*;
use deno_core::{FastString, ToV8};
use std::sync::atomic::Ordering;

deno_core::extension!(
    worker_wire_test,
    deps = [host_v8_worker_codec],
    ops = [op_worker_recv_message, op_worker_terminate],
    esm_entry_point = "ext:worker_wire_test/bridge.js",
    esm = ["ext:worker_wire_test/bridge.js" = {
        source = r#"
        import { core } from 'ext:core/mod.js';
        import { encodeMessage, decodeMessage, postMessage } from 'ext:host_v8_worker_codec/00_worker_codec.js';
        globalThis.__send = postMessage;
        globalThis.__encode = encodeMessage;
        globalThis.__decode = wire => core.deserialize(wire, {forStorage: true});
        globalThis.__reserve = core.ops.op_worker_reserve_message;
        globalThis.__commit = core.ops.op_worker_commit_message;
        globalThis.__abort = core.ops.op_worker_abort_message;
        globalThis.__next = core.ops.op_worker_recv_message;
        globalThis.__terminate = core.ops.op_worker_terminate;
        globalThis.check = (condition, detail) => {if (!condition) throw new Error(detail);};
    "#
    }],
);

fn runtime() -> JsRuntime {
    crate::icu::install_icu_data();
    JsRuntime::new(RuntimeOptions {
        extensions: vec![host_v8_worker_codec::init(), worker_wire_test::init()],
        ..Default::default()
    })
}

fn main_handle() -> (WorkerHandle, WorkerMessageReceiver) {
    let (tx, rx) = worker_message_channel();
    let (_from, from_rx) = worker_outbound_channel();
    let (_error, error_rx) = worker_error_channel();
    let (lifecycle, _) = worker_lifecycle_channel();
    (
        WorkerHandle {
            tx_to_worker: tx,
            rx_from_worker: Arc::new(tokio::sync::Mutex::new(from_rx)),
            rx_errors: Arc::new(tokio::sync::Mutex::new(error_rx)),
            timer_backgrounded_tx: lifecycle,
            join_handle: None,
            terminated: false,
            isolate_handle: Arc::new(std::sync::Mutex::new(None)),
            terminate_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
        rx,
    )
}

fn exec(rt: &mut JsRuntime, code: impl Into<String>) {
    rt.execute_script("worker-wire-test", FastString::from(code.into()))
        .unwrap();
}

fn receive(rt: &mut JsRuntime, wire: WorkerWire) {
    deno_core::scope!(scope, rt);
    let value = wire.to_v8(scope).unwrap();
    let global = scope.get_current_context().global(scope);
    let key = v8::String::new(scope, "__wire").unwrap();
    global.create_data_property(scope, key.into(), value);
}

#[test]
fn owned_wire_preserves_values_and_survives_sender_isolate_drop() {
    let executor = crate::tests::support::v8_test_executor();
    let _enter = executor.as_ref().map(|executor| executor.enter());
    let mut sender = runtime();
    let (handle, mut rx) = main_handle();
    let usage = handle.tx_to_worker.data.usage.clone();
    sender.op_state().borrow_mut().put(handle);
    exec(
        &mut sender,
        r#"
        const buffer = new ArrayBuffer(32);
        new Uint8Array(buffer)[7] = 42;
        const source = { buffer, view: new Uint8Array(buffer, 7, 3),
            date: new Date(1234), map: new Map(), set: new Set(), n: NaN, z: -0,
            undef: undefined, big: 1234567890123456789n, re: /mini/giu };
        source.self = source; source.map.set(source, source.view); source.set.add(source);
        source.boxed = [new Number(NaN), new Boolean(false), new String('mini'), Object(123n)];
        source.error = new TypeError('mini', {cause: {code: 42}});
        __send(source);
        new Uint8Array(buffer)[7] = 99;
        check(buffer.byteLength === 32, 'public ArrayBuffer was detached');
    "#,
    );
    assert_eq!(usage.items.load(Ordering::Acquire), 1);
    assert!(
        usage.bytes.load(Ordering::Acquire) < 1024,
        "reservation was not shrunk"
    );
    let Some(WorkerMessage::Message(wire)) = rx.try_recv().ok() else {
        panic!("wire missing")
    };
    assert_eq!(usage.bytes.load(Ordering::Acquire), 0);
    // A real thread hand-off: only an owned backing store crosses, no V8 handle.
    let wire = std::thread::spawn(move || wire).join().unwrap();
    drop(sender);
    let mut receiver = runtime();
    receive(&mut receiver, wire);
    exec(
        &mut receiver,
        r#"
        const value = __decode(__wire);
        check(value.self === value && value.map.get(value) === value.view && value.set.has(value), 'graph identity');
        check(value.view.buffer === value.buffer && value.view.byteOffset === 7 && value.view[0] === 42, 'views/copy');
        check(value.date instanceof Date && +value.date === 1234 && value.re.flags === 'giu', 'types');
        check(Number.isNaN(value.n) && Object.is(value.z, -0) && 'undef' in value && value.undef === undefined, 'numbers/undefined');
        check(value.big === 1234567890123456789n, 'BigInt');
        check(Number.isNaN(value.boxed[0].valueOf()) && value.boxed[1].valueOf() === false &&
              value.boxed[2].valueOf() === 'mini' && value.boxed[3].valueOf() === 123n, 'boxed primitives');
        check(value.error instanceof TypeError && value.error.message === 'mini' && value.error.cause.code === 42, 'Error/cause');
    "#,
    );
}

#[test]
fn rejected_clones_and_invalid_backing_stores_release_all_reservations() {
    let executor = crate::tests::support::v8_test_executor();
    let _enter = executor.as_ref().map(|executor| executor.enter());
    let mut rt = runtime();
    let (handle, _rx) = main_handle();
    let usage = handle.tx_to_worker.data.usage.clone();
    rt.op_state().borrow_mut().put(handle);
    exec(
        &mut rt,
        r#"
        for (let i = 0; i < 100; i++) {
            let error;
            try { __send({bad: () => {}}); } catch (caught) { error = caught; }
            check(error?.name === 'DataCloneError', 'clone error type');
        }
        for (const bad of [new Uint8Array(new ArrayBuffer(1024 * 1024), 1, 3),
                            new Uint8Array(new SharedArrayBuffer(16)),
                            new Uint8Array(new ArrayBuffer(16, {maxByteLength: 32})),
                            new Uint8Array(0)]) {
            const {ticket} = __reserve(64);
            let rejected = false;
            try { __commit(ticket, bad); } catch (_) { rejected = true; }
            finally { __abort(ticket); }
            check(rejected, 'unsafe or disproportionate backing store accepted');
        }
    "#,
    );
    assert_eq!(usage.items.load(Ordering::Acquire), 0);
    assert_eq!(usage.bytes.load(Ordering::Acquire), 0);
    assert!(
        rt.op_state()
            .borrow()
            .borrow::<WorkerReservations>()
            .pending
            .is_empty()
    );
}

#[test]
fn full_or_closed_queues_reject_before_running_a_getter_with_a_worker_error() {
    let executor = crate::tests::support::v8_test_executor();
    let _enter = executor.as_ref().map(|executor| executor.enter());
    let mut rt = runtime();
    let (handle, rx) = main_handle();
    for _ in 0..WORKER_MESSAGE_QUEUE_CAPACITY {
        handle
            .tx_to_worker
            .send(WorkerMessage::Message("x".into()))
            .unwrap();
    }
    rt.op_state().borrow_mut().put(handle);
    exec(
        &mut rt,
        r#"(() => {
            let calls = 0, error;
            try { __send({get value() {calls++; return 42;}}); } catch (caught) {error = caught;}
            check(calls === 0, 'serialization ran before admission');
            check(error?.name === 'WorkerError', 'unregistered WorkerError');
        })();"#,
    );
    drop(rx);
    exec(
        &mut rt,
        "let calls = 0; try {__send({get x(){calls++;}});} catch (e) {check(e.name === 'WorkerError', 'closed error');} check(calls === 0, 'closed getter');",
    );
}

#[test]
fn old_reservation_cannot_be_committed_into_a_replacement_worker() {
    let executor = crate::tests::support::v8_test_executor();
    let _enter = executor.as_ref().map(|executor| executor.enter());
    let mut rt = runtime();
    let (old, _old_rx) = main_handle();
    let old_usage = old.tx_to_worker.data.usage.clone();
    rt.op_state().borrow_mut().put(old);
    exec(&mut rt, "globalThis.pending = __reserve(1024);");
    let (replacement, mut new_rx) = main_handle();
    rt.op_state().borrow_mut().put(replacement);
    exec(
        &mut rt,
        r#"
        let error;
        try {__commit(pending.ticket, __encode({old: true}, 1024));} catch (caught) {error = caught;}
        finally {__abort(pending.ticket);}
        check(error?.name === 'WorkerError', 'old send succeeded');
    "#,
    );
    assert!(new_rx.try_recv().is_err());
    assert_eq!(old_usage.items.load(Ordering::Acquire), 0);
    assert_eq!(old_usage.bytes.load(Ordering::Acquire), 0);
}

#[test]
fn native_ready_send_preserves_getter_exceptions_and_allows_safe_reentry() {
    let executor = crate::tests::support::v8_test_executor();
    let _enter = executor.as_ref().map(|executor| executor.enter());
    let mut rt = runtime();
    let (handle, mut rx) = main_handle();
    let usage = handle.tx_to_worker.data.usage.clone();
    rt.op_state().borrow_mut().put(handle);
    exec(
        &mut rt,
        r#"
        const thrown = {sentinel: true};
        let caught;
        try {__send({get value(){throw thrown;}});} catch (error) {caught = error;}
        check(caught === thrown, 'getter exception identity changed');
        __send({get order(){ __send({order:'inner'}); return 'outer'; }});
    "#,
    );
    for expected in ["inner", "outer"] {
        let WorkerMessage::Message(wire) = rx.try_recv().unwrap() else {
            panic!("missing reentrant message")
        };
        receive(&mut rt, wire);
        exec(
            &mut rt,
            format!("check(__decode(__wire).order === '{expected}', 'reentrant order');"),
        );
    }
    exec(
        &mut rt,
        r#"
        let error;
        try {__send({get value(){__terminate(); return 42;}});} catch (caught) {error = caught;}
        check(error?.name === 'WorkerError', 'retired send was accepted');
    "#,
    );
    assert_eq!(usage.items.load(Ordering::Acquire), 0);
    assert_eq!(usage.bytes.load(Ordering::Acquire), 0);
    assert!(rx.data.try_recv().is_err());
}

#[test]
fn pre_ready_commit_detaches_only_the_private_wire_and_preserves_its_owned_store() {
    let executor = crate::tests::support::v8_test_executor();
    let _enter = executor.as_ref().map(|executor| executor.enter());
    let mut rt = runtime();
    let (handle, mut rx) = main_handle();
    rt.op_state().borrow_mut().put(handle);
    exec(
        &mut rt,
        r#"
        const original = new ArrayBuffer(1024);
        new Uint8Array(original)[7] = 42;
        const privateWire = __encode({original}, 4096);
        const {ticket} = __reserve(privateWire.byteLength);
        try {__commit(ticket, privateWire);} finally {__abort(ticket);}
        check(privateWire.byteLength === 0, 'private wire was retained in sender JS');
        check(original.byteLength === 1024, 'game buffer was detached');
        new Uint8Array(original)[7] = 99;
    "#,
    );
    let WorkerMessage::Message(wire) = rx.try_recv().unwrap() else {
        panic!("missing private wire")
    };
    drop(rt);
    let mut receiver = runtime();
    receive(&mut receiver, wire);
    exec(
        &mut receiver,
        "check(new Uint8Array(__decode(__wire).original)[7] === 42, 'detached private store lost its copy');",
    );
}

#[test]
fn decoding_releases_its_external_memory_charge_on_success_and_error() {
    let executor = crate::tests::support::v8_test_executor();
    let _enter = executor.as_ref().map(|executor| executor.enter());
    let mut rt = runtime();
    deno_core::scope!(scope, rt);
    let original = v8::ArrayBuffer::new(scope, 1024);
    let bytes = deno_core::serialize_for_storage(scope, original.into(), 4096, None)
        .unwrap()
        .unwrap();
    // V8 150 heap statistics report backing stores, not the embedder counter.
    // Query the actual counter with a zero adjustment.
    // Core also creates external strings for its host-object symbol. Compare
    // the wrapper against the same direct core operation, including metadata.
    deno_core::deserialize_for_storage(scope, &bytes).unwrap();
    let before_reference = scope.adjust_amount_of_external_allocated_memory(0);
    deno_core::deserialize_for_storage(scope, &bytes).unwrap();
    let reference_delta = scope.adjust_amount_of_external_allocated_memory(0) - before_reference;
    let before = scope.adjust_amount_of_external_allocated_memory(0);
    let value = WorkerWire::from_owned(bytes)
        .unwrap()
        .decode(scope)
        .unwrap();
    assert!(value.is_array_buffer());
    let after = scope.adjust_amount_of_external_allocated_memory(0);
    assert_eq!(
        after - before,
        reference_delta,
        "wire must add no retained charge beyond core's result"
    );
    let invalid = [1, 2, 3];
    let before_error_reference = scope.adjust_amount_of_external_allocated_memory(0);
    assert!(deno_core::deserialize_for_storage(scope, &invalid).is_err());
    let reference_error_delta =
        scope.adjust_amount_of_external_allocated_memory(0) - before_error_reference;
    let before_error = scope.adjust_amount_of_external_allocated_memory(0);
    assert!(
        WorkerWire::from_owned(invalid.into())
            .unwrap()
            .decode(scope)
            .is_err()
    );
    assert_eq!(
        scope.adjust_amount_of_external_allocated_memory(0),
        before_error + reference_error_delta
    );
}

#[test]
fn lifecycle_and_message_to_v8_have_distinct_internal_shapes() {
    let executor = crate::tests::support::v8_test_executor();
    let _enter = executor.as_ref().map(|executor| executor.enter());
    let mut rt = runtime();
    let payload = {
        deno_core::scope!(scope, rt);
        let value = v8::String::new(scope, "payload").unwrap();
        WorkerWire::from_owned(
            deno_core::serialize_for_storage(scope, value.into(), 1024, None)
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    };
    for event in [
        WorkerInbound::Lifecycle {
            backgrounded: true,
            elapsed_micros: 2500,
        },
        WorkerInbound::Message { data: payload },
    ] {
        {
            deno_core::scope!(scope, rt);
            let value = event.to_v8(scope).unwrap();
            let key = v8::String::new(scope, "event").unwrap();
            scope
                .get_current_context()
                .global(scope)
                .create_data_property(scope, key.into(), value);
        }
        exec(
            &mut rt,
            r#"
            if (!Array.isArray(event)) {
                check(event.backgrounded === true && event.elapsedMicros === 2500 && !('data' in event), 'lifecycle envelope');
            } else {
                check(event.length === 1 && event[0] === 'payload' && !('backgrounded' in event), 'wire envelope');
            }
        "#,
        );
    }
}

#[tokio::test]
async fn terminate_drains_a_pending_receive_and_a_retained_handle() {
    let mut rt = runtime();
    let (mut handle, _rx) = main_handle();
    let (tx, rx) = worker_outbound_channel();
    let usage = tx.usage.clone();
    handle.rx_from_worker = Arc::new(tokio::sync::Mutex::new(rx));
    rt.op_state().borrow_mut().put(handle);
    exec(
        &mut rt,
        "globalThis.received = 'pending'; __next().then(value => received = value);",
    );
    for _ in 0..16 {
        futures::future::poll_fn(|cx| {
            let _ = rt.poll_event_loop(cx, PollEventLoopOptions::default());
            std::task::Poll::Ready(())
        })
        .await;
        tokio::task::yield_now().await;
        if rt
            .op_state()
            .borrow()
            .borrow::<WorkerHandle>()
            .rx_from_worker
            .try_lock()
            .is_err()
        {
            break;
        }
    }
    // The lazy receive now owns the receiver mutex. Termination cannot lock it.
    assert!(
        rt.op_state()
            .borrow()
            .borrow::<WorkerHandle>()
            .rx_from_worker
            .try_lock()
            .is_err()
    );
    for _ in 0..32 {
        tx.send("queued-private-wire".into()).unwrap();
    }
    rt.op_state()
        .borrow()
        .borrow::<WorkerHandle>()
        .force_terminate();
    for _ in 0..16 {
        futures::future::poll_fn(|cx| {
            let _ = rt.poll_event_loop(cx, PollEventLoopOptions::default());
            std::task::Poll::Ready(())
        })
        .await;
        tokio::task::yield_now().await;
    }
    assert_eq!(usage.items.load(Ordering::Acquire), 0);
    assert_eq!(usage.bytes.load(Ordering::Acquire), 0);
    exec(
        &mut rt,
        "check(received === null || received === undefined, 'terminated receive delivered: ' + String(received));",
    );
    assert!(matches!(tx.try_reserve(1), Err(WorkerReserveError::Closed)));
    // The handle is still in OpState, but owns no queued payloads.
    assert!(rt.op_state().borrow().has::<WorkerHandle>());
}

#[test]
fn worker_to_main_uses_the_same_copy_contract_limits_and_error_registration() {
    let executor = crate::tests::support::v8_test_executor();
    let _enter = executor.as_ref().map(|executor| executor.enter());
    let mut rt = runtime();
    let (_tx, rx) = worker_message_channel();
    let (tx_to_main, mut to_main) = worker_outbound_channel();
    let usage = tx_to_main.usage.clone();
    let (tx_errors, _errors) = worker_error_channel();
    let (_lifecycle_tx, lifecycle_rx) = worker_lifecycle_channel();
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    rt.op_state().borrow_mut().put(WorkerCtx {
        tx_to_main,
        tx_errors,
        rx_from_main: Arc::new(tokio::sync::Mutex::new(rx)),
        timer_prelude_rx: Arc::new(std::sync::Mutex::new(lifecycle_rx.clone())),
        timer_backgrounded_rx: Arc::new(tokio::sync::Mutex::new(lifecycle_rx)),
        terminate_requested: stopped.clone(),
    });
    exec(
        &mut rt,
        r#"
        const source = new Uint8Array(1024 * 1024); source[1023] = 77;
        __send({source}); source[1023] = 99;
        check(source.byteLength === 1024 * 1024, 'reverse send detached source');
    "#,
    );
    let wire = to_main.try_recv().unwrap();
    receive(&mut rt, wire);
    exec(
        &mut rt,
        "check(__decode(__wire).source[1023] === 77, 'reverse clone aliases source');",
    );
    assert_eq!(usage.bytes.load(Ordering::Acquire), 0);
    exec(
        &mut rt,
        r#"
        for (let i = 0; i < 4; i++) __send(new Uint8Array(16 * 1024 * 1024 - 64));
        let refused = false;
        try { __send(new Uint8Array(1024)); } catch (_) { refused = true; }
        check(refused, 'aggregate byte limit was bypassed');
    "#,
    );
    assert!(usage.bytes.load(Ordering::Acquire) <= MAX_WORKER_QUEUED_BYTES);
    assert!(usage.bytes.load(Ordering::Acquire) > 63 * 1024 * 1024);
    drop(to_main);
    assert_eq!(usage.items.load(Ordering::Acquire), 0);
    assert_eq!(usage.bytes.load(Ordering::Acquire), 0);
    stopped.store(true, Ordering::Release);
    exec(
        &mut rt,
        r#"
        let getterCalls = 0, error;
        try { __send({get value() {getterCalls++; return 42;}}); } catch (caught) { error = caught; }
        check(getterCalls === 0 && error?.name === 'WorkerError', 'reverse stop/error contract');
    "#,
    );
}
