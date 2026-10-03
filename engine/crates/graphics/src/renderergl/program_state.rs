//! What a linked program says about itself that the facade cannot know: its uniform blocks, where the uniforms in
//! them sit, a uniform's value and a fragment output's location. `getActiveUniformBlockName`,
//! `getActiveUniformBlockParameter`, `getUniformIndices`, `getActiveUniforms`, `getUniform` and `getFragDataLocation`
//! are answered here, from the driver, through the generic state query (`frame_wire::sync::gl_state`).
//!
//! Every answer is an envelope: `{"v":<value>}`, or `{"e":<GL error>}` when the specification makes the call an
//! error (an unlinked program, or a location the program no longer has, is INVALID_OPERATION, a block or uniform
//! index past the count INVALID_VALUE, a pname WebGL does not allow INVALID_ENUM). The facade unwraps it and raises the error on the context, so the driver is
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

/// How `getUniform` reads a uniform, and what the facade makes of its words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UniformKind {
    Float,
    /// An `int` or a sampler.
    Int,
    Unsigned,
    Bool,
}

impl UniformKind {
    fn tag(self) -> &'static str {
        match self {
            UniformKind::Float => "f",
            UniformKind::Int => "i",
            UniformKind::Unsigned => "u",
            UniformKind::Bool => "b",
        }
    }
}

/// A uniform type's kind and component count (ES 3.0 table 2.10), or `None` for a type that is not a uniform's.
fn uniform_shape(ty: u32) -> Option<(UniformKind, usize)> {
    use UniformKind::*;
    Some(match ty {
        0x1406 => (Float, 1),           // FLOAT
        0x8B50 => (Float, 2),           // FLOAT_VEC2
        0x8B51 => (Float, 3),           // FLOAT_VEC3
        0x8B52 => (Float, 4),           // FLOAT_VEC4
        0x8B5A => (Float, 4),           // FLOAT_MAT2
        0x8B5B => (Float, 9),           // FLOAT_MAT3
        0x8B5C => (Float, 16),          // FLOAT_MAT4
        0x8B65 | 0x8B67 => (Float, 6),  // FLOAT_MAT2x3, FLOAT_MAT3x2
        0x8B66 | 0x8B69 => (Float, 8),  // FLOAT_MAT2x4, FLOAT_MAT4x2
        0x8B68 | 0x8B6A => (Float, 12), // FLOAT_MAT3x4, FLOAT_MAT4x3
        0x1404 => (Int, 1),             // INT
        0x8B53 => (Int, 2),             // INT_VEC2
        0x8B54 => (Int, 3),             // INT_VEC3
        0x8B55 => (Int, 4),             // INT_VEC4
        0x1405 => (Unsigned, 1),        // UNSIGNED_INT
        0x8DC6 => (Unsigned, 2),        // UNSIGNED_INT_VEC2
        0x8DC7 => (Unsigned, 3),        // UNSIGNED_INT_VEC3
        0x8DC8 => (Unsigned, 4),        // UNSIGNED_INT_VEC4
        0x8B56 => (Bool, 1),            // BOOL
        0x8B57 => (Bool, 2),            // BOOL_VEC2
        0x8B58 => (Bool, 3),            // BOOL_VEC3
        0x8B59 => (Bool, 4),            // BOOL_VEC4
        // SAMPLER_2D, _3D, _CUBE, _2D_SHADOW, _2D_ARRAY, _2D_ARRAY_SHADOW, _CUBE_SHADOW, and the INT_ and
        // UNSIGNED_INT_ samplers: the texture unit, an int.
        0x8B5E | 0x8B5F | 0x8B60 | 0x8B62 | 0x8DC1 | 0x8DC4 | 0x8DC5 | 0x8DCA | 0x8DCB | 0x8DCC
        | 0x8DCF | 0x8DD2 | 0x8DD3 | 0x8DD4 | 0x8DD7 => (Int, 1),
        _ => return None,
    })
}

/// A uniform's name without a trailing array index: `a[2]`, `a[0]` and `a` are all `a` (the driver names an array by
/// its first element), and `s[1].f` is itself.
fn without_index(name: &str) -> &str {
    if let Some(open) = name.strip_suffix(']').and_then(|inner| inner.rfind('[')) {
        let digits = &name[open + 1..name.len() - 1];
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            return &name[..open];
        }
    }
    name
}

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
    if query == gl_state::LINK_RESULT {
        return link_result(gl, program);
    }
    unsafe {
        if !gl.get_program_link_status(program) {
            return error(INVALID_OPERATION);
        }
        let blocks = || gl.get_program_parameter_i32(program, glow::ACTIVE_UNIFORM_BLOCKS) as u32;
        match query {
            gl_state::ACTIVE_UNIFORM_BLOCK_NAME => {
                if extra >= blocks() {
                    return error(INVALID_VALUE);
                }
                value(json_string(
                    &gl.get_active_uniform_block_name(program, extra),
                ))
            }
            gl_state::ACTIVE_UNIFORM_BLOCK_PARAMETER => {
                if extra >= blocks() {
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
            gl_state::UNIFORM_VALUE => uniform_value(gl, program, name),
            gl_state::FRAG_DATA_LOCATION => {
                value(gl.get_frag_data_location(program, name).to_string())
            }
            _ => "null".to_string(),
        }
    }
}

/// `LINK_RESULT`: whether the program linked, and every attribute location it consumes. An active attribute takes its
/// location and, for a matrix, one more per further column (ES 3.0 2.11.5); a built-in input such as `gl_VertexID` has
/// no location and takes none.
fn link_result(gl: &glow::Context, program: glow::Program) -> String {
    // SAFETY: `gl` is the current context and `program` one of its programs, which the caller resolved.
    unsafe {
        if !gl.get_program_link_status(program) {
            return value("[false,[]]".to_string());
        }
        let count = gl
            .get_program_parameter_i32(program, glow::ACTIVE_ATTRIBUTES)
            .max(0) as u32;
        let mut locations = Vec::new();
        for index in 0..count {
            let Some(attribute) = gl.get_active_attribute(program, index) else {
                continue;
            };
            let Some(location) = gl.get_attrib_location(program, &attribute.name) else {
                continue;
            };
            let taken = attribute_columns(attribute.atype) * attribute.size.max(1) as u32;
            locations.extend((0..taken).map(|column| location + column));
        }
        locations.sort_unstable();
        locations.dedup();
        value(format!("[true,{}]", array(locations)))
    }
}

/// The locations an attribute of `ty` takes: a matrix one per column, anything else one.
fn attribute_columns(ty: u32) -> u32 {
    match ty {
        0x8B5A | 0x8B65 | 0x8B66 => 2, // FLOAT_MAT2, FLOAT_MAT2x3, FLOAT_MAT2x4
        0x8B5B | 0x8B67 | 0x8B68 => 3, // FLOAT_MAT3, FLOAT_MAT3x2, FLOAT_MAT3x4
        0x8B5C | 0x8B69 | 0x8B6A => 4, // FLOAT_MAT4, FLOAT_MAT4x2, FLOAT_MAT4x3
        _ => 1,
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

/// `getUniform`: the value of the uniform the facade looked its location up by `name`, in the link that gave it -- the
/// facade refuses a location from an earlier link, so the name finds the same location now. The type is the active
/// uniform's that `name` is an element or a member of.
fn uniform_value(gl: &glow::Context, program: glow::Program, name: &str) -> String {
    // SAFETY: `gl` is the current context and `program` a linked program of it, which `answer` checked. Every read
    // writes at most 16 components (a mat4), the size of the buffers it is given.
    unsafe {
        let Some(found) = gl.get_uniform_location(program, name) else {
            return error(INVALID_OPERATION);
        };
        let base = without_index(name);
        let uniforms = gl
            .get_program_parameter_i32(program, glow::ACTIVE_UNIFORMS)
            .max(0) as u32;
        let shape = (0..uniforms)
            .filter_map(|index| gl.get_active_uniform(program, index))
            .find(|uniform| without_index(&uniform.name) == base)
            .and_then(|uniform| uniform_shape(uniform.utype));
        let Some((kind, count)) = shape else {
            return error(INVALID_OPERATION);
        };
        let words: Vec<u32> = match kind {
            UniformKind::Float => {
                let mut v = [0f32; 16];
                gl.get_uniform_f32(program, &found, &mut v);
                v[..count].iter().map(|f| f.to_bits()).collect()
            }
            UniformKind::Int | UniformKind::Bool => {
                let mut v = [0i32; 16];
                gl.get_uniform_i32(program, &found, &mut v);
                v[..count].iter().map(|&i| i as u32).collect()
            }
            UniformKind::Unsigned => {
                let mut v = [0u32; 16];
                gl.get_uniform_u32(program, &found, &mut v);
                v[..count].to_vec()
            }
        };
        value(format!("[\"{}\",{}]", kind.tag(), array(words)))
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

    /// A matrix attribute takes one location per column; everything else one.
    #[test]
    fn a_matrix_attribute_takes_a_location_per_column() {
        assert_eq!(attribute_columns(0x8B52), 1); // FLOAT_VEC4
        assert_eq!(attribute_columns(0x8B5A), 2); // FLOAT_MAT2
        assert_eq!(attribute_columns(0x8B67), 3); // FLOAT_MAT3x2: three columns of two
        assert_eq!(attribute_columns(0x8B66), 2); // FLOAT_MAT2x4: two columns of four
        assert_eq!(attribute_columns(0x8B5C), 4); // FLOAT_MAT4
        assert_eq!(attribute_columns(0x1404), 1); // INT
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_string("ub_view"), "\"ub_view\"");
        assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
    }

    #[test]
    fn an_array_element_and_its_first_element_name_the_same_uniform() {
        assert_eq!(without_index("a[2]"), "a");
        assert_eq!(without_index("a[0]"), "a");
        assert_eq!(without_index("a"), "a");
        assert_eq!(without_index("s[1].f"), "s[1].f");
        assert_eq!(without_index("s[1].v[3]"), "s[1].v");
        assert_eq!(without_index("a[]"), "a[]");
        assert_eq!(without_index("a[x]"), "a[x]");
    }

    #[test]
    fn every_uniform_type_has_a_kind_and_a_component_count() {
        use UniformKind::*;
        assert_eq!(uniform_shape(0x1406), Some((Float, 1)));
        assert_eq!(uniform_shape(0x8B5C), Some((Float, 16)));
        assert_eq!(uniform_shape(0x8B6A), Some((Float, 12)));
        assert_eq!(uniform_shape(0x8B55), Some((Int, 4)));
        assert_eq!(uniform_shape(0x8DC8), Some((Unsigned, 4)));
        assert_eq!(uniform_shape(0x8B59), Some((Bool, 4)));
        assert_eq!(uniform_shape(0x8DD7), Some((Int, 1)));
        assert_eq!(uniform_shape(0x1234), None);
        // No uniform has more components than the buffers `uniform_value` reads into.
        for ty in 0x1400..0x9000 {
            if let Some((_, count)) = uniform_shape(ty) {
                assert!((1..=16).contains(&count), "{ty:#x}");
            }
        }
    }

    #[test]
    fn envelopes_are_one_of_value_or_error() {
        assert_eq!(value(array([1u32, 2, 3])), "{\"v\":[1,2,3]}");
        assert_eq!(value(array(Vec::<u32>::new())), "{\"v\":[]}");
        assert_eq!(error(INVALID_VALUE), "{\"e\":1281}");
    }
}
