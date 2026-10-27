use deno_core::{AsyncResult, BufView, JsRuntime, Resource, RuntimeOptions, extension};
use deno_error::JsErrorBox;
use migo_services::network::resources::CancelFlag;
use std::{cell::Cell, rc::Rc};

extension!(stream_test,
    esm_entry_point = "ext:stream_test/setup.js",
    esm = [
        dir "src/web",
        "06_stream.js",
        "ext:stream_test/setup.js" = { source = "import { ReadableStream } from 'ext:stream_test/06_stream.js'; globalThis.TestStream = ReadableStream;" },
    ],
);

#[derive(Clone, Copy)]
enum Mode {
    Chunk,
    Pending,
    Eof,
}
struct Probe {
    mode: Cell<Mode>,
    closed: Cell<usize>,
    cancel: CancelFlag,
}
impl Resource for Probe {
    fn read(self: Rc<Self>, _: usize) -> AsyncResult<BufView> {
        Box::pin(async move {
            match self.mode.replace(Mode::Eof) {
                Mode::Chunk => Ok(BufView::from(vec![1, 2, 3])),
                Mode::Eof => Ok(BufView::empty()),
                Mode::Pending => {
                    self.cancel
                        .until_cancelled(std::future::pending::<()>())
                        .await;
                    Err(JsErrorBox::generic("read cancelled"))
                }
            }
        })
    }
    fn close(self: Rc<Self>) {
        self.closed.set(self.closed.get() + 1);
        self.cancel.cancel();
    }
}
fn fixture(mode: Mode) -> (JsRuntime, Rc<Probe>, u32) {
    crate::icu::install_icu_data();
    let mut rt = JsRuntime::new(RuntimeOptions {
        extensions: vec![stream_test::init()],
        ..Default::default()
    });
    let probe = Rc::new(Probe {
        mode: Cell::new(mode),
        closed: Cell::new(0),
        cancel: CancelFlag::default(),
    });
    let rid = rt
        .op_state()
        .borrow_mut()
        .resource_table
        .add_rc(probe.clone());
    rt.execute_script(
        "stream",
        format!("globalThis.stream = new TestStream({rid}); globalThis.events = [];"),
    )
    .unwrap();
    (rt, probe, rid)
}

#[tokio::test]
async fn callback_failure_releases_the_resource_without_an_outer_request_wrapper() {
    let (mut rt, probe, rid) = fixture(Mode::Chunk);
    rt.execute_script("callback-error", "stream.pull(new Uint8Array(8), () => {throw new Error('callback failed');}).catch(e => events.push(e.message));").unwrap();
    rt.run_event_loop(Default::default()).await.unwrap();
    assert_eq!(probe.closed.get(), 1);
    assert!(!rt.op_state().borrow().resource_table.has(rid));
    rt.execute_script(
        "result",
        "if (events.join() !== 'callback failed') throw new Error('lost callback error');",
    )
    .unwrap();
}

#[tokio::test]
async fn closing_a_pending_core_read_rejects_and_releases_once() {
    let (mut rt, probe, rid) = fixture(Mode::Pending);
    rt.execute_script("cancel-read", "stream.pull(new Uint8Array(8), () => events.push('data')).catch(e => events.push(e.message)); stream.cancel(); stream.cancel();").unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        rt.run_event_loop(Default::default()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(probe.closed.get(), 1);
    assert!(!rt.op_state().borrow().resource_table.has(rid));
    rt.execute_script("result", "if (events.length !== 1 || events[0] === 'data') throw new Error('cancel delivered a chunk');").unwrap();
}

#[tokio::test]
async fn cancellation_while_eof_profile_is_pending_suppresses_end_callback() {
    let (mut rt, probe, rid) = fixture(Mode::Eof);
    rt.execute_script("eof-profile", "stream.pull(new Uint8Array(8), value => events.push(value), () => new Promise(resolve => globalThis.profileDone = resolve)).catch(e => events.push(e.message));").unwrap();
    rt.run_event_loop(Default::default()).await.unwrap();
    assert!(rt.op_state().borrow().resource_table.has(rid));
    rt.execute_script("cancel-profile", "stream.cancel(); profileDone();")
        .unwrap();
    rt.run_event_loop(Default::default()).await.unwrap();
    assert_eq!(probe.closed.get(), 1);
    rt.execute_script(
        "result",
        "if (events.length !== 0) throw new Error('end callback after cancellation');",
    )
    .unwrap();
}
