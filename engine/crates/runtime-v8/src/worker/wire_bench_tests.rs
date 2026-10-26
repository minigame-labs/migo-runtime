//! Same-cohort two-isolate RTT experiment; no benchmark branch ships.
use super::*;

struct JsonChannel {
    sender: WorkerQueueSender<String>,
    receiver: Arc<tokio::sync::Mutex<WorkerQueueReceiver<String>>>,
}

#[op2(fast)]
fn op_worker_bench_json_post(
    state: &mut OpState,
    #[string] value: &str,
) -> Result<(), WorkerError> {
    if value.len() > MAX_WORKER_MESSAGE_BYTES {
        return Err(WorkerError::Message("Worker message too large".into()));
    }
    let channel = state.borrow::<JsonChannel>();
    let permit = channel
        .sender
        .try_reserve(value.len())
        .map_err(worker_reserve_error)?;
    channel
        .sender
        .send_reserved(value.to_owned(), permit)
        .map_err(|error| worker_queue_error(error, "JSON benchmark channel closed"))
}

#[op2(async(lazy), fast)]
#[string]
async fn op_worker_bench_json_recv(state: Rc<RefCell<OpState>>) -> Option<String> {
    let receiver = state.borrow().borrow::<JsonChannel>().receiver.clone();
    receiver.lock().await.recv().await
}

#[op2(fast)]
fn op_worker_bench_now() -> f64 {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
        * 1e6
}

deno_core::extension!(
    worker_rtt_bench,
    deps = [host_v8_worker_codec],
    ops = [
        op_worker_bench_json_post,
        op_worker_bench_json_recv,
        op_worker_bench_now,
        op_worker_recv_message,
        op_worker_inner_recv_message
    ],
    esm_entry_point = "ext:worker_rtt_bench/bridge.js",
    esm = ["ext:worker_rtt_bench/bridge.js" = {
        source = r#"
        import { core } from 'ext:core/mod.js';
        import { postMessage, decodeMessage } from 'ext:host_v8_worker_codec/00_worker_codec.js';
        const { op_worker_bench_json_post, op_worker_bench_json_recv, op_worker_bench_now,
            op_worker_recv_message, op_worker_inner_recv_message } = core.ops;
        globalThis.__bench = async (transport, role, kind, count, length) => {
            const send = transport === 'json' ? value => op_worker_bench_json_post(JSON.stringify(value)) : postMessage;
            const receive = transport === 'json' ? async () => JSON.parse(await op_worker_bench_json_recv()) :
                role === 'main' ? async () => decodeMessage(await op_worker_recv_message()) :
                async () => decodeMessage(await op_worker_inner_recv_message());
            if (role === 'worker') {
                for (let index = 0; index < count + 32; index++) send(await receive());
                return;
            }
            let value;
            if (kind === 'tiny') value = {op:'tick', sequence:7, values:[1,2,3], ready:true};
            else if (kind === 'string') value = 'k'.repeat(length);
            else { value = new ArrayBuffer(length); new Uint8Array(value)[length - 1] = 77; }
            const samples = [];
            const started = op_worker_bench_now();
            for (let index = 0; index < count + 32; index++) {
                const before = op_worker_bench_now();
                send(value); const result = await receive();
                const elapsed = op_worker_bench_now() - before;
                if (index >= 32) samples.push(elapsed);
                const valid = kind === 'tiny' ? result.sequence === 7 && result.values[2] === 3 :
                    kind === 'string' ? result.length === length && result.charCodeAt(length - 1) === 107 :
                    result.byteLength === length && new Uint8Array(result)[length - 1] === 77;
                if (!valid) throw new Error('RTT payload changed');
            }
            const elapsed = op_worker_bench_now() - started;
            samples.sort((a,b) => a-b);
            globalThis.__metrics = {transport,kind,count,length,p50_us:samples[Math.floor(count/2)],
                p95_us:samples[Math.min(count-1,Math.floor(count*.95))],elapsed_including_warmup_us:elapsed};
        };
    "#
    }],
);

fn runtime() -> JsRuntime {
    crate::icu::install_icu_data();
    let limits = crate::V8LimitsConfig::default();
    JsRuntime::new(RuntimeOptions {
        extensions: vec![host_v8_worker_codec::init(), worker_rtt_bench::init()],
        create_params: Some(
            v8::Isolate::create_params()
                .heap_limits(limits.initial_heap_size, limits.max_heap_size),
        ),
        ..Default::default()
    })
}

#[test]
#[ignore = "fresh-process release experiment; run through compare_worker_transports.py"]
fn structured_worker_roundtrip_experiment() {
    let transport = std::env::var("MIGO_WORKER_BENCH_TRANSPORT").unwrap_or_else(|_| "clone".into());
    let kind = std::env::var("MIGO_WORKER_BENCH_KIND").unwrap_or_else(|_| "tiny".into());
    let count: usize = std::env::var("MIGO_WORKER_BENCH_COUNT")
        .unwrap_or_else(|_| "1000".into())
        .parse()
        .unwrap();
    let length: usize = std::env::var("MIGO_WORKER_BENCH_BYTES")
        .unwrap_or_else(|_| "0".into())
        .parse()
        .unwrap();
    assert!(["json", "clone"].contains(&transport.as_str()));
    assert!(["tiny", "string", "buffer"].contains(&kind.as_str()));
    assert!(
        transport != "json" || kind != "buffer",
        "JSON has no equivalent ArrayBuffer semantics"
    );
    assert!(count >= 10 && length < MAX_WORKER_MESSAGE_BYTES);
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _enter = executor.enter();
    let mut main = runtime();
    let (to_worker, from_main) = worker_message_channel();
    let (to_main, from_worker) = worker_outbound_channel();
    let (errors, error_rx) = worker_error_channel();
    let (lifecycle, lifecycle_rx) = worker_lifecycle_channel();
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_ctx = WorkerCtx {
        tx_to_main: to_main,
        tx_errors: errors,
        rx_from_main: Arc::new(tokio::sync::Mutex::new(from_main)),
        timer_prelude_rx: Arc::new(std::sync::Mutex::new(lifecycle_rx.clone())),
        timer_backgrounded_rx: Arc::new(tokio::sync::Mutex::new(lifecycle_rx)),
        terminate_requested: stopped.clone(),
    };
    let (json_to_worker, json_from_main) =
        worker_queue(WORKER_MESSAGE_QUEUE_CAPACITY, MAX_WORKER_QUEUED_BYTES);
    let (json_to_main, json_from_worker) =
        worker_queue(WORKER_MESSAGE_QUEUE_CAPACITY, MAX_WORKER_QUEUED_BYTES);
    main.op_state().borrow_mut().put(JsonChannel {
        sender: json_to_worker,
        receiver: Arc::new(tokio::sync::Mutex::new(json_from_worker)),
    });
    let worker_script = format!("__bench('{transport}', 'worker', '{kind}', {count}, {length});");
    let worker = std::thread::spawn(move || {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = executor.enter();
        let mut worker = runtime();
        worker.op_state().borrow_mut().put(worker_ctx);
        worker.op_state().borrow_mut().put(JsonChannel {
            sender: json_to_main,
            receiver: Arc::new(tokio::sync::Mutex::new(json_from_main)),
        });
        worker
            .execute_script(
                "worker-rtt-loop",
                deno_core::FastString::from(worker_script),
            )
            .unwrap();
        executor
            .block_on(worker.run_event_loop(Default::default()))
            .unwrap();
    });
    main.op_state().borrow_mut().put(WorkerHandle {
        tx_to_worker: to_worker,
        rx_from_worker: Arc::new(tokio::sync::Mutex::new(from_worker)),
        rx_errors: Arc::new(tokio::sync::Mutex::new(error_rx)),
        timer_backgrounded_tx: lifecycle,
        join_handle: Some(worker),
        terminated: false,
        isolate_handle: Arc::new(std::sync::Mutex::new(None)),
        terminate_requested: stopped,
    });
    main.execute_script(
        "main-rtt-loop",
        deno_core::FastString::from(format!(
            "__bench('{transport}', 'main', '{kind}', {count}, {length});"
        )),
    )
    .unwrap();
    executor
        .block_on(main.run_event_loop(Default::default()))
        .unwrap();
    let result = main
        .execute_script("worker-rtt-result", "JSON.stringify(__metrics)")
        .unwrap();
    deno_core::scope!(scope, main);
    let result = v8::Local::new(scope, result).to_rust_string_lossy(scope);
    println!("MIGO_WORKER_BENCH {result}");
}
