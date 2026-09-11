//! Actual-op regressions. JS is never deliberately raced with an IO worker.
use super::*;
use deno_core::{JsRuntime, RuntimeOptions};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

struct FileFixture {
    runtime: Option<JsRuntime>,
    scheduler: Arc<IoScheduler>,
    path: PathBuf,
    root: PathBuf,
    executor: tokio::runtime::Runtime,
}

impl FileFixture {
    fn new(bytes: &[u8]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "migo-file-byob-{}-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        for name in ["code", "user", "cache", "tmp"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        let path = root.join("user/file.bin");
        std::fs::write(&path, bytes).unwrap();
        let mut host = super::tests::zip_test_host_state();
        host.vfs = Some(Arc::new(VirtualFS::new(
            root.join("code"),
            root.join("user"),
            root.join("cache"),
            root.join("tmp"),
        )));
        deno_core::extension!(
            file_byob_bridge,
            deps = [host_v8_file],
            esm_entry_point = "ext:file_byob_bridge/bridge.js",
            esm = ["ext:file_byob_bridge/bridge.js" = {
                source = r#"
                    import { BaseFileManager } from "ext:host_v8_file/02_file_manager.js";
                    globalThis.fs = BaseFileManager;
                "#
            },],
        );
        let mut extensions = crate::main_extensions(host);
        extensions.push(file_byob_bridge::init());
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions,
            ..Default::default()
        });
        let scheduler = get_scheduler(&runtime.op_state().borrow());
        let rid = scheduler
            .domain()
            .open_file(&path, OpenFlag::ReadWrite, None, None)
            .unwrap();
        runtime
            .execute_script("file-fixture", format!("globalThis.fd = {rid};"))
            .unwrap();
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        Self {
            runtime: Some(runtime),
            scheduler,
            path,
            root,
            executor,
        }
    }

    fn script(&mut self, source: &'static str) {
        let _guard = self.executor.enter();
        self.runtime
            .as_mut()
            .unwrap()
            .execute_script("file-byob-test", source)
            .unwrap();
    }

    fn drain(&mut self) {
        self.executor
            .block_on(
                self.runtime
                    .as_mut()
                    .unwrap()
                    .run_event_loop(Default::default()),
            )
            .unwrap();
    }
}

impl Drop for FileFixture {
    fn drop(&mut self) {
        self.scheduler.close();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn async_fd_write_snapshots_before_lazy_poll() {
    let mut fixture = FileFixture::new(&[0; 4]);
    fixture.script(
        r#"
        globalThis.input = new Uint8Array([1, 2, 3, 4]);
        globalThis.pending = Deno.core.ops.op_write_file(fd, input, null, null, null);
    "#,
    );
    // Old async(lazy) code has not formed a worker-side slice yet. Assert
    // this before modifying JS memory, so RED proves timing without a race.
    assert_eq!(fixture.scheduler.metrics().delegated_runs, 0);
    fixture.script("input.fill(9);");
    fixture.drain();
    assert_eq!(std::fs::read(&fixture.path).unwrap(), [1, 2, 3, 4]);
}

#[test]
fn async_raw_read_returns_owned_bytes_without_writing_the_js_view() {
    let mut fixture = FileFixture::new(&[1, 2, 3]);
    fixture.script(
        r#"
        globalThis.target = new Uint8Array([165, 165, 165, 165]);
        globalThis.result = null;
        Deno.core.ops.op_read_fd_into(fd, target, null).then(bytes => { result = bytes; });
    "#,
    );
    fixture.drain();
    // The worker has finished before inspecting the original view.
    fixture.script(
        r#"
        if (target.some(value => value !== 165)) throw new Error('worker changed JS destination');
        if (!(result instanceof Uint8Array) || result.join(',') !== '1,2,3')
            throw new Error('read did not return owned bytes');
        if (result.byteLength !== 3 || result.buffer.byteLength !== 4 ||
            new Uint8Array(result.buffer)[3] !== 0) throw new Error('invalid staging capacity');
    "#,
    );
}

#[test]
fn sync_file_ops_reject_shared_and_resizable_backing() {
    let mut fixture = FileFixture::new(&[1, 2, 3, 4]);
    fixture.script(r#"
        for (const buffer of [new SharedArrayBuffer(4), new ArrayBuffer(4, {maxByteLength: 8})]) {
            const view = new Uint8Array(buffer);
            let failures = 0;
            try { Deno.core.ops.op_read_fd_into_sync(fd, view, 0n); } catch (_) { ++failures; }
            try { Deno.core.ops.op_write_file_sync(fd, view, null, null, 0n); } catch (_) { ++failures; }
            if (failures !== 2) throw new Error('unsafe backing accepted');
        }
    "#);
}

#[test]
fn async_path_write_snapshots_only_the_subview_before_lazy_poll() {
    let mut fixture = FileFixture::new(&[0; 3]);
    fixture.script(r#"
        globalThis.input = new Uint8Array([99, 1, 2, 3, 99]);
        Deno.core.ops.op_write_or_append_file('/user/file.bin', input.subarray(1, 4), null, null, false, false);
    "#);
    assert_eq!(fixture.scheduler.metrics().delegated_runs, 0);
    fixture.script("input.fill(9);");
    fixture.drain();
    assert_eq!(std::fs::read(&fixture.path).unwrap(), [1, 2, 3]);
}

#[test]
fn public_read_commits_before_callbacks_and_preserves_identity_and_tail() {
    let mut fixture = FileFixture::new(&[1, 2, 3]);
    fixture.script(
        r#"
        globalThis.target = new Uint8Array(8).fill(165);
        globalThis.events = [];
        globalThis.done = false;
        // Neither an own byteLength property nor a replaced set may redirect IO.
        Object.defineProperty(target.buffer, 'byteLength', { value: 800 });
        Uint8Array.prototype.set = () => { throw new Error('user set called'); };
        fs.read({fd, arrayBuffer: target.buffer, offset: 2,
            success(result) {
                if (result.arrayBuffer !== target.buffer || result.bytesRead !== 3 ||
                    target.join(',') !== '165,165,1,2,3,165,165,165') throw new Error('bad commit');
                events.push('success');
            },
            fail() { events.push('fail'); },
            complete() { events.push('complete'); },
        }).then(result => {
            if (result.arrayBuffer !== target.buffer || result.bytesRead !== 3)
                throw new Error('bad result');
            events.push('promise'); done = true;
        });
    "#,
    );
    fixture.drain();
    fixture.script("if (!done || events.join(',') !== 'success,complete,promise') throw new Error('settlement order');");
}

#[test]
fn public_read_zero_length_seeks_eof_and_invalid_fd_settle_correctly() {
    let mut fixture = FileFixture::new(b"abcdef");
    fixture.script(r#"
        globalThis.done = false;
        (async () => {
            const empty = new ArrayBuffer(0);
            const zero = await fs.read({fd, arrayBuffer: empty, position: 3});
            if (zero.bytesRead !== 0 || zero.arrayBuffer !== empty) throw new Error('zero result');
            const bytes = new Uint8Array(5).fill(165);
            const read = await fs.read({fd, arrayBuffer: bytes.buffer});
            if (read.bytesRead !== 3 || bytes.join(',') !== '100,101,102,165,165') throw new Error('zero seek');
            const eof = await fs.read({fd, arrayBuffer: bytes.buffer});
            if (eof.bytesRead !== 0 || bytes.join(',') !== '100,101,102,165,165') throw new Error('EOF');
            let failed = 0, completed = 0, rejected = false;
            try {
                await fs.read({fd: 2147483647, arrayBuffer: empty,
                    fail() { ++failed; }, complete() { ++completed; }});
            } catch (_) { rejected = true; }
            if (!rejected || failed !== 1 || completed !== 1) throw new Error('invalid fd settlement');
            done = true;
        })();
    "#);
    fixture.drain();
    fixture.script("if (!done) throw new Error('not finished');");
}

#[test]
fn public_read_detached_before_completion_rejects_even_for_empty_reads() {
    let mut fixture = FileFixture::new(&[1, 2, 3]);
    fixture.script(
        r#"
        globalThis.done = false;
        (async () => {
            for (const length of [4, 0]) {
                const buffer = new ArrayBuffer(length);
                let succeeded = 0, failed = 0, completed = 0, rejected = false;
                const pending = fs.read({fd, arrayBuffer: buffer, position: 0,
                    success() { ++succeeded; }, fail() { ++failed; }, complete() { ++completed; }});
                buffer.transfer();
                try { await pending; } catch (_) { rejected = true; }
                if (!rejected || succeeded || failed !== 1 || completed !== 1)
                    throw new Error('detached destination settled successfully');
            }
            done = true;
        })();
    "#,
    );
    fixture.drain();
    fixture.script("if (!done) throw new Error('not finished');");
}

#[test]
fn all_async_file_buffer_ops_and_sync_path_write_reject_unsafe_backing() {
    let mut fixture = FileFixture::new(&[1, 2, 3, 4]);
    fixture.script(r#"
        globalThis.done = false;
        (async () => {
            for (const buffer of [new SharedArrayBuffer(4), new ArrayBuffer(4, {maxByteLength: 8})]) {
                const view = new Uint8Array(buffer);
                for (const operation of [
                    () => Deno.core.ops.op_read_fd_into(fd, view, 0n),
                    () => Deno.core.ops.op_write_file(fd, view, null, null, 0n),
                    () => Deno.core.ops.op_write_or_append_file('/user/file.bin', view, null, null, false, false),
                    () => Deno.core.ops.op_write_or_append_file_sync('/user/file.bin', view, null, null, false, false),
                ]) {
                    let rejected = false;
                    try { await operation(); } catch (_) { rejected = true; }
                    if (!rejected) throw new Error('unsafe backing accepted');
                }
            }
            done = true;
        })();
    "#);
    fixture.drain();
    fixture.script("if (!done) throw new Error('not finished');");
    assert_eq!(std::fs::read(&fixture.path).unwrap(), [1, 2, 3, 4]);
}

#[test]
fn sync_read_and_async_string_write_keep_position_and_window_semantics() {
    let mut fixture = FileFixture::new(b"abcdef");
    fixture.script(r#"
        const bytes = new Uint8Array(8).fill(165);
        const read = fs.readSync({fd, arrayBuffer: bytes.buffer, offset: 2, length: 3, position: 1});
        if (read.arrayBuffer !== bytes.buffer || read.bytesRead !== 3 ||
            bytes.join(',') !== '165,165,98,99,100,165,165,165') throw new Error('sync read');
        Deno.core.ops.op_write_file(fd, null, '5859', 'hex', 2n).then(count => {
            if (count !== 2n) throw new Error('write count');
        });
    "#);
    fixture.drain();
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"abXYef");
}

#[test]
fn async_read_releases_js_backing_before_submission_and_runtime_drop() {
    use deno_core::v8;
    let mut fixture = FileFixture::new(&[1, 2, 3]);
    let target_len = fixture.scheduler.policy().small_read_bytes + 1;
    let runtime = fixture.runtime.as_mut().unwrap();
    let value = runtime
        .execute_script(
            "buffer",
            format!("globalThis.target = new Uint8Array({target_len}).fill(165); target"),
        )
        .unwrap();
    let backing = {
        deno_core::scope!(scope, runtime);
        let local = v8::Local::new(scope, value);
        let view = v8::Local::<v8::Uint8Array>::try_from(local).unwrap();
        view.get_backing_store().unwrap()
    };
    backing.assert_use_count_eq(2); // V8 and this test.
    fixture.script("Deno.core.ops.op_read_fd_into(fd, target, null);");
    backing.assert_use_count_eq(2); // The lazy op must retain no JS backing store.
    fixture.drain();
    assert_eq!(fixture.scheduler.metrics().delegated_runs, 1);
    backing.assert_use_count_eq(2); // Completed work must not retain it.
    fixture.script("Deno.core.ops.op_read_fd_into(fd, target, 0n);");
    backing.assert_use_count_eq(2);
    // Cancel a second unpolled request by dropping its actual consumer runtime.
    drop(fixture.runtime.take());
    fixture.scheduler.close();
    backing.assert_use_count_eq(1);
}

#[test]
fn public_read_end_offset_clamps_length_and_closed_domain_rejects() {
    let mut fixture = FileFixture::new(&[1, 2, 3]);
    fixture.script(r#"
        globalThis.done = false;
        (async () => {
            const bytes = new Uint8Array(3).fill(165);
            const empty = await fs.read({fd, arrayBuffer: bytes.buffer, offset: 3, length: 100, position: 1});
            if (empty.bytesRead !== 0) throw new Error('end offset');
            const read = await fs.read({fd, arrayBuffer: bytes.buffer, offset: 1, length: 100});
            if (read.bytesRead !== 2 || bytes.join(',') !== '165,2,3') throw new Error('length clamp');
            done = true;
        })();
    "#);
    fixture.drain();
    fixture.script("if (!done) throw new Error('not finished');");
    fixture.scheduler.close();
    fixture.script(
        r#"
        globalThis.rejected = false;
        fs.read({fd, arrayBuffer: new ArrayBuffer(4)}).catch(() => { rejected = true; });
    "#,
    );
    fixture.drain();
    fixture.script("if (!rejected) throw new Error('closed IO succeeded');");
}
