//! What deno_core makes of an op argument, read from V8 rather than from its
//! source.
//!
//! On iOS Performance+ the engine's JavaScript calls its ops in WebKit, where
//! no deno_core converts the arguments: the producer's `op-args.mjs` restates
//! the conversion for each argument kind, and a lane that answers an op has to
//! see the number, string or bytes the Rust op body would have. Restating is
//! where drift hides -- `#[smi]` and a plain `u32` convert differently, a
//! `#[string]` that is handed a number becomes the empty string rather than an
//! error, and a `#[buffer]` takes a `Uint8Array` and nothing else -- so the
//! rules are read from V8: every value in `fixtures/op-arg-probes.js` goes
//! through a real op of each kind below, and the answers must be exactly the
//! checked-in `fixtures/op-arg-answers.json`. The producer's side of the same
//! file is `test/op-args.test.mjs`, which runs without cargo; between the two,
//! the producer converts as V8 does.
//!
//! The kinds are `scripts/lib/runtime_ops.py`'s names for the conversions
//! (`param_kind`), which is also what `op-args.mjs` tags each function with.
//!
//! When deno_core changes a rule, this test fails first. Regenerate the file
//! with `MIGO_OP_ARGS_BLESS=1` and the producer's test then shows what
//! `op-args.mjs` has to follow.

use std::path::PathBuf;

use deno_core::{FastString, JsBuffer, JsRuntime, RuntimeOptions, op2, serde_json};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[op2]
#[string]
fn op_probe_smi_u32(#[smi] value: u32) -> String {
    value.to_string()
}

#[op2]
#[string]
fn op_probe_u32(value: u32) -> String {
    value.to_string()
}

#[op2]
#[string]
fn op_probe_smi_i32(#[smi] value: i32) -> String {
    value.to_string()
}

#[op2]
#[string]
fn op_probe_i32(value: i32) -> String {
    value.to_string()
}

#[op2]
#[string]
fn op_probe_u8(value: u8) -> String {
    value.to_string()
}

#[op2]
#[string]
fn op_probe_smi_u8(#[smi] value: u8) -> String {
    value.to_string()
}

#[op2]
#[string]
fn op_probe_smi_u64(#[smi] value: u64) -> String {
    value.to_string()
}

#[op2]
#[string]
fn op_probe_bigint_u64(#[bigint] value: u64) -> String {
    value.to_string()
}

#[op2]
#[string]
fn op_probe_bigint_option_u64(#[bigint] value: Option<u64>) -> String {
    value.map_or_else(|| "none".to_string(), |value| value.to_string())
}

/// `#[smi] u16`: what `op_ws_close` takes for the close code.
#[op2]
#[string]
fn op_probe_smi_u16(#[smi] value: u16) -> String {
    value.to_string()
}

/// `#[smi] Option<u32>`: what `op_fetch` takes for the client and the body
/// resource it may be given instead of bytes.
#[op2]
#[string]
fn op_probe_smi_option_u32(#[smi] value: Option<u32>) -> String {
    value.map_or_else(|| "none".to_string(), |value| value.to_string())
}

/// `Option<u32>` with no attribute: what `op_resize_canvas` takes for each
/// dimension, because content assigns `canvas.width` and `canvas.height`
/// separately and either may be absent. A different conversion from the `#[smi]`
/// form above -- `to_u32_option` rather than `to_i32_option` -- which is the
/// whole reason the two are separate kinds.
#[op2]
#[string]
fn op_probe_option_u32(value: Option<u32>) -> String {
    value.map_or_else(|| "none".to_string(), |value| value.to_string())
}

#[op2]
#[string]
fn op_probe_bool(value: bool) -> String {
    value.to_string()
}

#[op2]
#[string]
fn op_probe_f32(value: f32) -> String {
    format!("{:08x}", value.to_bits())
}

#[op2]
#[string]
fn op_probe_f64(value: f64) -> String {
    format!("{:016x}", value.to_bits())
}

#[op2]
#[string]
fn op_probe_string(#[string] value: String) -> String {
    serde_json::to_string(&value).expect("a string is JSON")
}

#[op2]
#[string]
fn op_probe_option_string(#[string] value: Option<String>) -> String {
    value.map_or_else(
        || "none".to_string(),
        |value| serde_json::to_string(&value).expect("a string is JSON"),
    )
}

#[op2]
#[string]
fn op_probe_buffer(#[buffer] value: JsBuffer) -> String {
    format!("bytes:{}", hex(&value))
}

#[op2]
#[string]
fn op_probe_option_buffer(#[buffer] value: Option<JsBuffer>) -> String {
    value.map_or_else(
        || "none".to_string(),
        |value| format!("bytes:{}", hex(&value)),
    )
}

#[op2]
#[string]
fn op_probe_u32_list(#[buffer(copy)] value: Vec<u32>) -> String {
    let words: Vec<String> = value.iter().map(u32::to_string).collect();
    format!("u32s:{}", words.join(","))
}

deno_core::extension!(
    op_arg_probes,
    ops = [
        op_probe_smi_u32,
        op_probe_u32,
        op_probe_smi_i32,
        op_probe_i32,
        op_probe_u8,
        op_probe_smi_u8,
        op_probe_smi_u64,
        op_probe_bigint_u64,
        op_probe_bigint_option_u64,
        op_probe_smi_option_u32,
        op_probe_option_u32,
        op_probe_smi_u16,
        op_probe_bool,
        op_probe_f32,
        op_probe_f64,
        op_probe_string,
        op_probe_option_string,
        op_probe_buffer,
        op_probe_option_buffer,
        op_probe_u32_list,
    ]
);

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Every probe through every kind, as `{kind: {label: answer}}`. An answer is
/// the op's canonical rendering of what it received, or `ErrorName: message`
/// for a conversion V8 refused.
fn deno_answers() -> serde_json::Value {
    let fixture = std::fs::read_to_string(
        repository()
            .join("platforms/apple/WebContent/PerformancePlus/test/fixtures/op-arg-probes.js"),
    )
    .expect("the probe fixture");
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: vec![op_arg_probes::init()],
        ..Default::default()
    });
    runtime
        .execute_script("op-arg-probes.js", FastString::from(fixture))
        .expect("the fixture evaluates");
    let answers = runtime
        .execute_script(
            "<op-args-agreement>",
            FastString::from_static(
                r#"
                (() => {
                  const ops = Deno.core.ops;
                  const kinds = {
                    smi_u32: ops.op_probe_smi_u32,
                    u32: ops.op_probe_u32,
                    smi_i32: ops.op_probe_smi_i32,
                    i32: ops.op_probe_i32,
                    u8: ops.op_probe_u8,
                    smi_u8: ops.op_probe_smi_u8,
                    smi_u64: ops.op_probe_smi_u64,
                    bigint_u64: ops.op_probe_bigint_u64,
                    option_bigint_u64: ops.op_probe_bigint_option_u64,
                    option_smi_u32: ops.op_probe_smi_option_u32,
                    option_u32: ops.op_probe_option_u32,
                    smi_u16: ops.op_probe_smi_u16,
                    bool: ops.op_probe_bool,
                    f32: ops.op_probe_f32,
                    f64: ops.op_probe_f64,
                    string: ops.op_probe_string,
                    option_string: ops.op_probe_option_string,
                    u8_buffer: ops.op_probe_buffer,
                    option_u8_buffer: ops.op_probe_option_buffer,
                    u32_buffer: ops.op_probe_u32_list,
                  };
                  const out = {};
                  for (const [kind, op] of Object.entries(kinds)) {
                    const row = {};
                    for (const [label, value] of globalThis.OP_ARG_PROBES) {
                      try {
                        row[label] = op(value);
                      } catch (error) {
                        row[label] = `${error.name}: ${error.message}`;
                      }
                    }
                    out[kind] = row;
                  }
                  return JSON.stringify(out);
                })()
                "#,
            ),
        )
        .expect("the probes run");
    deno_core::scope!(scope, runtime);
    let local = deno_core::v8::Local::new(scope, answers);
    serde_json::from_str(&local.to_rust_string_lossy(scope)).expect("the answers are JSON")
}

fn answers_path() -> PathBuf {
    repository()
        .join("platforms/apple/WebContent/PerformancePlus/test/fixtures/op-arg-answers.json")
}

#[test]
fn v8_converts_every_probe_as_the_checked_in_answers_say() {
    let live = deno_answers();
    let path = answers_path();
    if std::env::var_os("MIGO_OP_ARGS_BLESS").is_some() {
        let mut text = serde_json::to_string_pretty(&live).expect("JSON");
        text.push('\n');
        std::fs::write(&path, text).expect("write the answers");
    }
    let golden: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("fixtures/op-arg-answers.json"),
    )
    .expect("the answers are JSON");

    let kinds = live.as_object().expect("kinds");
    let mut differences = Vec::new();
    let mut compared = 0usize;
    for (kind, row) in kinds {
        let checked_in = golden.get(kind);
        for (label, answer) in row.as_object().expect("a row") {
            compared += 1;
            let other = checked_in.and_then(|row| row.get(label));
            if other != Some(answer) {
                differences.push(format!(
                    "  {kind} / {label}: V8 {answer}, checked in {}",
                    other.map_or("<none>".to_string(), |value| value.to_string())
                ));
            }
        }
    }
    assert_eq!(
        golden.as_object().map(|kinds| kinds.len()),
        Some(kinds.len()),
        "the checked-in answers name kinds V8 was not asked about"
    );
    assert!(
        differences.is_empty(),
        "{} of {compared} conversions differ from fixtures/op-arg-answers.json \
         (regenerate with MIGO_OP_ARGS_BLESS=1 if deno_core changed a rule):\n{}",
        differences.len(),
        differences.join("\n")
    );
}
