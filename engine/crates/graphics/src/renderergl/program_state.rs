//! What a linked program says about itself that the facade cannot know: its uniform blocks, and where the uniforms
//! in them sit. `getActiveUniformBlockName`, `getActiveUniformBlockParameter`, `getUniformIndices` and
//! `getActiveUniforms` are answered here, from the driver, through the generic state query
//! (`frame_wire::sync::gl_state`).
//!
//! Every answer is an envelope: `{"v":<value>}`, or `{"e":<GL error>}` when the specification makes the call an
//! error (an unlinked program is INVALID_OPERATION, a block or uniform index past the count INVALID_VALUE, a pname
//! WebGL does not allow INVALID_ENUM). The facade unwraps it and raises the error on the context, so the driver is
//! only ever asked a question it can answer and no stray driver error is left behind for a later `getError`.

use glow::HasContext;
use shared::protocol::render_cmd::gl_state;

const INVALID_ENUM: u32 = 0x0500;
const INVALID_VALUE: u32 = 0x0501;
const INVALID_OPERATION: u32 = 0x0502;

const UNIFORM_TYPE: u32 = 0x8A37;
const UNIFORM_SIZE: u32 = 0x8A38;
const UNIFORM_BLOCK_INDEX: u32 = 0x8A3A;
const UNIFORM_OFFSET: u32 = 0x8A3B;
const UNIFORM_ARRAY_STRIDE: u32 = 0x8A3C;
const UNIFORM_MATRIX_STRIDE: u32 = 0x8A3D;
const UNIFORM_IS_ROW_MAJOR: u32 = 0x8A3E;

const UNIFORM_BLOCK_BINDING: u32 = 0x8A3F;
const UNIFORM_BLOCK_DATA_SIZE: u32 = 0x8A40;
const UNIFORM_BLOCK_ACTIVE_UNIFORMS: u32 = 0x8A42;
const UNIFORM_BLOCK_ACTIVE_UNIFORM_INDICES: u32 = 0x8A43;
const UNIFORM_BLOCK_REFERENCED_BY_VERTEX_SHADER: u32 = 0x8A44;
const UNIFORM_BLOCK_REFERENCED_BY_FRAGMENT_SHADER: u32 = 0x8A46;

/// `getUniformIndices` answers this for a name that is not an active uniform (`INVALID_INDEX`).
const INVALID_INDEX: u32 = u32::MAX;

fn value(json: String) -> String {
    format!("{{\"v\":{json}}}")
}

fn error(code: u32) -> String {
    format!("{{\"e\":{code}}}")
}

fn array<T: ToString>(items: impl IntoIterator<Item = T>) -> String {
    let items: Vec<String> = items.into_iter().map(|item| item.to_string()).collect();
    format!("[{}]", items.join(","))
}

/// A JSON string literal. GLSL identifiers cannot hold a quote, a backslash or a control character; escaping them
/// anyway means a driver that reports one cannot make the reply unparseable.
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Answers `query` (one of `gl_state::ACTIVE_UNIFORM_BLOCK_NAME` and its siblings) about `program`. `extra` and `name`
/// are the query's arguments, as `gl_state` documents them.
pub(super) fn answer(
    gl: &glow::Context,
    program: glow::Program,
    query: u32,
    extra: u32,
    name: &str,
) -> String {
    unsafe {
        if !gl.get_program_link_status(program) {
            return error(INVALID_OPERATION);
        }
        let blocks = gl.get_program_parameter_i32(program, glow::ACTIVE_UNIFORM_BLOCKS) as u32;
        match query {
            gl_state::ACTIVE_UNIFORM_BLOCK_NAME => {
                if extra >= blocks {
                    return error(INVALID_VALUE);
                }
                value(json_string(
                    &gl.get_active_uniform_block_name(program, extra),
                ))
            }
            gl_state::ACTIVE_UNIFORM_BLOCK_PARAMETER => {
                if extra >= blocks {
                    return error(INVALID_VALUE);
                }
                // The queried pname travels as decimal text: three numbers do not fit the two a state query has.
                let pname = name.trim().parse::<u32>().unwrap_or(0);
                block_parameter(gl, program, extra, pname)
            }
            gl_state::UNIFORM_INDICES => {
                // Names are joined by `\n`, which no identifier can contain.
                let names: Vec<&str> = name.split('\n').collect();
                let indices = gl.get_uniform_indices(program, &names);
                value(array(
                    indices.into_iter().map(|i| i.unwrap_or(INVALID_INDEX)),
                ))
            }
            gl_state::ACTIVE_UNIFORMS_PARAMETER => {
                let indices: Vec<u32> = match name
                    .split(',')
                    .map(|part| part.trim().parse::<u32>())
                    .collect::<Result<_, _>>()
                {
                    Ok(indices) => indices,
                    Err(_) => return error(INVALID_VALUE),
                };
                let uniforms = gl.get_program_parameter_i32(program, glow::ACTIVE_UNIFORMS) as u32;
                if indices.iter().any(|&i| i >= uniforms) {
                    return error(INVALID_VALUE);
                }
                uniforms_parameter(gl, program, &indices, extra)
            }
            _ => "null".to_string(),
        }
    }
}

fn block_parameter(gl: &glow::Context, program: glow::Program, block: u32, pname: u32) -> String {
    // SAFETY: `gl` is the current context and `program` a linked program of it, which `answer` checked.
    unsafe {
        match pname {
            UNIFORM_BLOCK_BINDING | UNIFORM_BLOCK_DATA_SIZE | UNIFORM_BLOCK_ACTIVE_UNIFORMS => {
                let n = gl.get_active_uniform_block_parameter_i32(program, block, pname);
                value((n as u32).to_string())
            }
            UNIFORM_BLOCK_ACTIVE_UNIFORM_INDICES => {
                let count = gl
                    .get_active_uniform_block_parameter_i32(
                        program,
                        block,
                        UNIFORM_BLOCK_ACTIVE_UNIFORMS,
                    )
                    .max(0) as usize;
                let mut indices = vec![0i32; count];
                if count > 0 {
                    gl.get_active_uniform_block_parameter_i32_slice(
                        program,
                        block,
                        pname,
                        &mut indices,
                    );
                }
                value(array(indices.into_iter().map(|i| i as u32)))
            }
            UNIFORM_BLOCK_REFERENCED_BY_VERTEX_SHADER
            | UNIFORM_BLOCK_REFERENCED_BY_FRAGMENT_SHADER => {
                let n = gl.get_active_uniform_block_parameter_i32(program, block, pname);
                value((n != 0).to_string())
            }
            _ => error(INVALID_ENUM),
        }
    }
}

fn uniforms_parameter(
    gl: &glow::Context,
    program: glow::Program,
    indices: &[u32],
    pname: u32,
) -> String {
    if !matches!(
        pname,
        UNIFORM_TYPE
            | UNIFORM_SIZE
            | UNIFORM_BLOCK_INDEX
            | UNIFORM_OFFSET
            | UNIFORM_ARRAY_STRIDE
            | UNIFORM_MATRIX_STRIDE
            | UNIFORM_IS_ROW_MAJOR
    ) {
        // UNIFORM_NAME_LENGTH is the one the driver has and WebGL leaves out.
        return error(INVALID_ENUM);
    }
    // The driver is asked about each uniform once, in order: content may list one uniform many times, or list more
    // entries than the program has uniforms, and ANGLE answers a list longer than the active uniform count with
    // zeros (found by asking for [2, 1, 0, 2] of a three-uniform program). `unique` is what the driver is asked;
    // each requested index is answered from it.
    let mut unique: Vec<u32> = indices.to_vec();
    unique.sort_unstable();
    unique.dedup();
    // SAFETY: `gl` is the current context and `program` a linked program of it; every index is below the active
    // uniform count, which `answer` checked.
    let raw = unsafe { gl.get_active_uniforms_parameter(program, &unique, pname) };
    let answer = |index: u32| {
        raw[unique
            .binary_search(&index)
            .expect("every index is in `unique`")]
    };
    match pname {
        UNIFORM_TYPE | UNIFORM_SIZE => value(array(indices.iter().map(|&i| answer(i) as u32))),
        UNIFORM_IS_ROW_MAJOR => value(array(indices.iter().map(|&i| answer(i) != 0))),
        _ => value(array(indices.iter().map(|&i| answer(i)))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_string("ub_view"), "\"ub_view\"");
        assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
    }

    #[test]
    fn envelopes_are_one_of_value_or_error() {
        assert_eq!(value(array([1u32, 2, 3])), "{\"v\":[1,2,3]}");
        assert_eq!(value(array(Vec::<u32>::new())), "{\"v\":[]}");
        assert_eq!(error(INVALID_VALUE), "{\"e\":1281}");
    }
}
