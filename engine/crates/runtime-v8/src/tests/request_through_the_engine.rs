//! `migo.request`, as content calls it, over the one scheme that needs no network.
//!
//! The request path has a part nothing else exercises: reading a response body in
//! chunks, merging it, and turning it into what the caller asked for (`text`, parsed
//! JSON, an ArrayBuffer). `fetch_through_the_service` covers the service's ops and
//! reads a body with `core.read` itself; the engine's own `request()` -- the code that
//! ships -- was never run against a body, because the SSRF filter refuses every address
//! a test could listen on. A `data:` URL is answered through all of it without a
//! connection, so this runs the real `migo.request` in a real runtime.
//!
//! What it found: since v0.9.10 every `request` that received a non-empty body failed
//! with `read data failed: TypeError: expected typed ArrayBufferView`, because the merged
//! body was handed on as an ArrayBuffer to the code that decodes a typed array.

#[cfg(test)]
mod request_tests {
    use deno_core::{FastString, JsRuntime, RuntimeOptions};

    use super::super::support::test_host_state;

    /// Run `source` (which fills `globalThis.log` and sets `globalThis.done` when it has seen
    /// everything it waits for), settle the event loop, then run `check` against the log. A
    /// failed check throws, with the log in the message.
    fn run(source: &str, check: &str) {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread executor");
        let _guard = executor.enter();
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(test_host_state()),
            ..Default::default()
        });
        runtime
            .execute_script(
                "<test:request>",
                FastString::from(format!("globalThis.log = [];\n{source}")),
            )
            .expect("script executes");
        executor
            .block_on(runtime.run_event_loop(Default::default()))
            .expect("the event loop settles");
        let check = format!(
            "(() => {{ const log = globalThis.log; \
             const fail = (why) => {{ throw new Error(why + ': ' + JSON.stringify(log)); }}; \
             {check} }})()"
        );
        runtime
            .execute_script("<test:request-check>", FastString::from(check))
            .expect("what content saw is what it should have seen");
    }

    /// The text of a body is the text.
    #[test]
    fn a_text_body_arrives_as_text() {
        run(
            r#"
            migo.request({
                url: "data:text/plain;base64,aGVsbG8gd29ybGQ=",
                dataType: "text",
                success: (res) => log.push({ ok: true, data: res.data, type: typeof res.data, status: res.statusCode, errMsg: res.errMsg }),
                fail: (err) => log.push({ ok: false, err: JSON.stringify(err) }),
            });
        "#,
            r#"
            if (log.length !== 1) fail("one callback");
            if (!log[0].ok) fail("the request failed");
            if (log[0].data !== "hello world" || log[0].type !== "string") fail("the body");
            if (log[0].status !== 200 || log[0].errMsg !== "request:ok") fail("the head");
        "#,
        );
    }

    /// JSON is parsed by default, and a body that is not JSON arrives as the text it is.
    #[test]
    fn a_json_body_is_parsed_and_one_that_is_not_json_stays_text() {
        run(
            r#"
            migo.request({
                url: "data:application/json,%7B%22a%22%3A%5B1%2C2%5D%7D",
                success: (res) => log.push({ kind: "json", data: res.data }),
                fail: (err) => log.push({ kind: "json", failed: JSON.stringify(err) }),
            });
            migo.request({
                url: "data:text/plain,not%20json",
                success: (res) => log.push({ kind: "text", data: res.data }),
                fail: (err) => log.push({ kind: "text", failed: JSON.stringify(err) }),
            });
        "#,
            r#"
            const json = log.find((entry) => entry.kind === "json");
            const text = log.find((entry) => entry.kind === "text");
            if (!json || json.failed) fail("the JSON request failed");
            if (JSON.stringify(json.data) !== '{"a":[1,2]}') fail("parsed");
            if (!text || text.failed) fail("the text request failed");
            if (text.data !== "not json") fail("unparsable JSON stays the text it is");
        "#,
        );
    }

    /// `responseType: "arraybuffer"` is an ArrayBuffer with the bytes in it.
    #[test]
    fn an_arraybuffer_body_arrives_as_an_arraybuffer() {
        run(
            r#"
            migo.request({
                url: "data:application/octet-stream;base64,AAEC/w==",
                responseType: "arraybuffer",
                success: (res) => log.push({
                    isBuffer: res.data instanceof ArrayBuffer,
                    bytes: res.data instanceof ArrayBuffer ? Array.from(new Uint8Array(res.data)) : null,
                }),
                fail: (err) => log.push({ failed: JSON.stringify(err) }),
            });
        "#,
            r#"
            if (log.length !== 1 || log[0].failed) fail("the request failed");
            if (!log[0].isBuffer) fail("not an ArrayBuffer");
            if (log[0].bytes.join(",") !== "0,1,2,255") fail("the bytes");
        "#,
        );
    }

    /// The body is read in 64 KiB chunks and merged: one that spans several arrives whole and in
    /// order.
    #[test]
    fn a_body_of_several_chunks_arrives_whole_and_in_order() {
        run(
            r#"
            const length = 200000;
            const bytes = new Uint8Array(length);
            for (let i = 0; i < length; i++) bytes[i] = (i * 7 + (i >> 8)) & 255;
            // The engine has no `btoa` (it installs no page globals); this is the usual encoder.
            const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            const parts = [];
            for (let i = 0; i < length; i += 3) {
                const n = (bytes[i] << 16) | ((bytes[i + 1] || 0) << 8) | (bytes[i + 2] || 0);
                parts.push(alphabet[(n >> 18) & 63] + alphabet[(n >> 12) & 63]
                    + (i + 1 < length ? alphabet[(n >> 6) & 63] : "=")
                    + (i + 2 < length ? alphabet[n & 63] : "="));
            }
            const url = "data:application/octet-stream;base64," + parts.join("");
            migo.request({
                url,
                responseType: "arraybuffer",
                success: (res) => {
                    const got = new Uint8Array(res.data);
                    let first = -1;
                    for (let i = 0; i < length; i++) if (got[i] !== bytes[i]) { first = i; break; }
                    log.push({ length: got.length, firstDifference: first });
                },
                fail: (err) => log.push({ failed: JSON.stringify(err) }),
            });
        "#,
            r#"
            if (log.length !== 1 || log[0].failed) fail("the request failed");
            if (log[0].length !== 200000) fail("the length");
            if (log[0].firstDifference !== -1) fail("the bytes differ");
        "#,
        );
    }

    /// An empty body is an empty body of the kind asked for, not `null`: `res.data.length` and
    /// `res.data.byteLength` are read without a check.
    #[test]
    fn an_empty_body_is_empty_not_null() {
        run(
            r#"
            migo.request({
                url: "data:text/plain,",
                dataType: "text",
                success: (res) => log.push({ kind: "text", data: res.data }),
                fail: (err) => log.push({ kind: "text", failed: JSON.stringify(err) }),
            });
            migo.request({
                url: "data:text/plain,",
                responseType: "arraybuffer",
                success: (res) => log.push({ kind: "buffer", isBuffer: res.data instanceof ArrayBuffer, length: res.data && res.data.byteLength }),
                fail: (err) => log.push({ kind: "buffer", failed: JSON.stringify(err) }),
            });
        "#,
            r#"
            const text = log.find((entry) => entry.kind === "text");
            const buffer = log.find((entry) => entry.kind === "buffer");
            if (!text || text.failed || text.data !== "") fail("an empty text body is the empty string");
            if (!buffer || buffer.failed || !buffer.isBuffer || buffer.length !== 0) {
                fail("an empty binary body is an empty ArrayBuffer");
            }
        "#,
        );
    }

    /// A callback of the app's that throws is the app's problem: the request still succeeded, so
    /// `fail` must not run, and `complete` runs once.
    #[test]
    fn a_success_callback_that_throws_does_not_turn_the_request_into_a_failure() {
        run(
            r#"
            migo.request({
                url: "data:text/plain,ok",
                dataType: "text",
                success: () => { log.push("success"); throw new Error("the app's own bug"); },
                fail: () => log.push("fail"),
                complete: () => log.push("complete"),
            });
        "#,
            r#"
            if (log.join(",") !== "success,complete") fail("success, then complete, and never fail");
        "#,
        );
    }

    /// And the other way: a `fail` that throws does not stop `complete`, and exactly one of the two
    /// runs.
    #[test]
    fn a_fail_callback_that_throws_still_runs_complete_once() {
        run(
            r#"
            migo.request({
                url: "ftp://example.com/",
                success: () => log.push("success"),
                fail: () => { log.push("fail"); throw new Error("the app's own bug"); },
                complete: () => log.push("complete"),
            });
        "#,
            r#"
            if (log.join(",") !== "fail,complete") fail("fail, then complete");
        "#,
        );
    }

    /// Arguments that make no request are a `fail`, not a throw out of `migo.request`: a game's
    /// request helper does not wrap every call in a try.
    #[test]
    fn a_call_that_cannot_make_a_request_fails_through_its_callbacks() {
        run(
            r#"
            const attempt = (label, options) => {
                try {
                    const task = migo.request(options);
                    log.push({ label, returned: typeof task });
                } catch (error) {
                    log.push({ label, threw: String(error && error.message) });
                }
            };
            for (const [label, options] of [
                ["bad method", { url: "https://example.com/", method: "FOO" }],
                ["method not a string", { url: "https://example.com/", method: 5 }],
                ["null options", null],
                ["header null", { url: "https://example.com/", header: null }],
                ["callbacks that are not functions", { url: "ftp://example.com/", success: 5, fail: "x", complete: {} }],
            ]) {
                attempt(label, options);
            }
            migo.request({
                url: "https://example.com/",
                method: "FOO",
                fail: (err) => log.push({ label: "bad method fail", errMsg: err.errMsg }),
                complete: () => log.push({ label: "bad method complete" }),
            });
        "#,
            r#"
            for (const entry of log) {
                if (entry.threw !== undefined) fail("migo.request threw for '" + entry.label + "': " + entry.threw);
            }
            const failure = log.find((entry) => entry.label === "bad method fail");
            const complete = log.find((entry) => entry.label === "bad method complete");
            if (!failure || failure.errMsg !== "request:fail") fail("an unsupported method is a request:fail");
            if (!complete) fail("complete runs");
        "#,
        );
    }
}
