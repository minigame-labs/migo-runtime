// The facade declared compression/TCP options but never sent them to the host.
// Execute the real embedded module with synthetic ops, including close-before-
// ready cleanup; no browser, network or duplicate facade implementation.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { SourceTextModule, SyntheticModule, createContext } from 'node:vm';

const source = await readFile(process.argv[2] || new URL('../../engine/crates/runtime-v8/src/network/07_websocket.js', import.meta.url), 'utf8');
const calls = [];
const closed = [];
const pending = [];
const cancels = [];
let nextCancel = 1000;
const context = createContext({ queueMicrotask, ArrayBuffer, Uint8Array });
let onReclaimed;
const core = { tryClose: rid => { closed.push(rid); onReclaimed?.(rid); } };
const ops = {
    op_fetch_upload_cancel_handle() {
        const rid = nextCancel++;
        cancels.push(rid);
        return rid;
    },
    op_ws_create(...args) {
        calls.push(args);
        return new Promise((resolve, reject) => pending.push({ resolve, reject }));
    },
    op_ws_next_event() { return new Promise(() => {}); },
    op_ws_send() { return Promise.resolve(); },
    op_ws_close() { return Promise.resolve(); },
};
function synthetic(exports) {
    return new SyntheticModule(Object.keys(exports), function () {
        for (const [name, value] of Object.entries(exports)) this.setExport(name, value);
    }, { context });
}
const imports = {
    'ext:core/mod.js': synthetic({ core }),
    'ext:core/ops': synthetic(ops),
    'ext:host_v8_base/02_async.js': synthetic({
        createListenerGroup() {
            const callbacks = new Set();
            return { on: callback => callbacks.add(callback), off: callback => callbacks.delete(callback),
                trigger: value => { for (const callback of callbacks) callback(value); } };
        },
        failMessage: (name, error) => `${name}:fail ${error.message}`,
    }),
    'ext:host_v8_network/00_binary.js': synthetic({
        toExactArrayBuffer: bytes => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
    }),
};
const module = new SourceTextModule(source, { context });
await module.link(name => {
    assert.ok(imports[name], `unexpected module import ${name}`);
    return imports[name];
});
await module.evaluate();
for (const [options, noDelay, deflate] of [
    [{}, true, false],
    [{ tcpNoDelay: true, perMessageDeflate: true }, true, true],
    [{ tcpNoDelay: false, perMessageDeflate: false }, false, false],
    [{ tcpNoDelay: 1, perMessageDeflate: 0 }, true, false],
]) {
    let opened = 0;
    const task = module.namespace.connectSocket({ url: 'wss://example.invalid/game', protocols: ['game'],
        header: { 'X-Game': 'test' }, timeout: 1234, ...options });
    task.onOpen(() => opened++);
    const args = calls.at(-1);
    task.close();
    const cancel = cancels.at(-1);
    assert.ok(cancel !== undefined && closed.includes(cancel),
        'closing before ready must cancel the handshake immediately');
    assert.equal(args.length, 7, 'options and the cancel handle must reach either native execution');
    assert.equal(args[6], cancel);
    assert.equal(args[0], 'wss://example.invalid/game');
    assert.equal(JSON.stringify(args[1]), '["game"]');
    assert.equal(JSON.stringify(args[2]), '[["X-Game","test"]]');
    assert.equal(args[3], 1234);
    assert.equal(args[4], noDelay);
    assert.equal(args[5], deflate);
    const rid = 100 + calls.length;
    const reclaimed = new Promise(resolve => { onReclaimed = resolve; });
    let timer;
    const deadline = new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('late connection was not reclaimed')), 1000);
    });
    try {
        pending.shift().resolve({ rid, protocol: 'game', extensions: '' });
        assert.equal(await Promise.race([reclaimed, deadline]), rid);
    } finally { clearTimeout(timer); }
    assert.equal(opened, 0, 'a pre-ready close must not reopen the task');
    assert.equal(closed.at(-1), rid, 'the late connection must be reclaimed');
}
// A normal connection releases its temporary cancel resource before onOpen;
// that cleanup must not close the newly established connection.
{
    let opened;
    const ready = new Promise(resolve => { opened = resolve; });
    const task = module.namespace.connectSocket({ url: 'wss://example.invalid/game' });
    const cancel = cancels.at(-1);
    task.onOpen(opened);
    pending.shift().resolve({ rid: 300, protocol: '', extensions: '' });
    await ready;
    assert.ok(closed.includes(cancel));
    assert.ok(!closed.includes(300), 'successful connect must remain open');
}
// A refused handshake releases its cancellation resource and reports once.
{
    let failed;
    const failure = new Promise(resolve => { failed = resolve; });
    module.namespace.connectSocket({ url: 'wss://example.invalid/game', fail: failed });
    const cancel = cancels.at(-1);
    pending.shift().reject(new Error('fixture refusal'));
    await failure;
    assert.ok(closed.includes(cancel), 'failed connect must not leak its cancel resource');
}
console.log('WebSocket facade: options, immediate handshake cancel and late-close cleanup PASS');
