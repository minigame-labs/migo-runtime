//! WebGL's shading language on a driver that is not a browser's.
//!
//! An ANGLE WebGL-compatible context already validates and initializes the shader. Other GLES contexts need the
//! missing initialization written into the source they compile. Only that driver source changes:
//! `getShaderSource` still answers what content set.

mod lexer;
mod preprocess;
mod zero_init;

pub(crate) use preprocess::Environment;

/// WebGL 2's default uniform-block layout, declared ahead of a GLSL ES 3.00 shader's first token.
const STD140_DEFAULT: &str = "layout(std140) uniform; ";

/// The source a driver should compile for the WebGL shader `source`: every variable it leaves without a value
/// initialized to zero, as WebGL requires and a driver need not do (see `zero_init`). `None` when the driver can take
/// `source` as it is -- its WebGL translator owns initialization, nothing is missing, or the shader will be refused.
pub(crate) fn for_driver(source: &str, env: &Environment) -> Option<String> {
    // ANGLE's WebGL compiler supplies this transformation itself and rejects the
    // implementation-reserved helper names our native-GLES workaround inserts.
    if env.extensions.contains("GL_ANGLE_webgl_compatibility") {
        return None;
    }
    let preprocessed = preprocess::preprocess(source, env).ok()?;
    let mut edits = zero_init::edits(&preprocessed.tokens, preprocessed.version).ok()?;
    // WebGL 2 lays every uniform block out as std140: "the initial state of compilation is as if
    // `layout(std140) uniform;` were declared". GLSL ES 3.00's own default is `shared`, whose layout
    // is the driver's, so content that fills a uniform buffer by the std140 rules, as WebGL promises
    // it may, would be read with other offsets wherever a driver packs differently. The default goes
    // in before the first token, on its line, so the driver's line numbers stay the source's; a
    // block that names a layout keeps it.
    if preprocessed.version >= 300
        && let Some((start, _)) = preprocessed.tokens.first().and_then(|token| token.span)
    {
        edits.insert(
            0,
            zero_init::Edit {
                start,
                end: start,
                text: STD140_DEFAULT.to_owned(),
            },
        );
    }
    if edits.is_empty() {
        return None;
    }
    let mut out =
        String::with_capacity(source.len() + edits.iter().map(|e| e.text.len()).sum::<usize>());
    let mut at = 0;
    for edit in &edits {
        out.push_str(&source[at..edit.start]);
        out.push_str(&edit.text);
        at = edit.end;
    }
    out.push_str(&source[at..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;
    use std::sync::LazyLock;

    static NONE: LazyLock<HashSet<String>> = LazyLock::new(HashSet::new);
    static FRAGMENT: LazyLock<Environment<'static>> = LazyLock::new(|| Environment {
        fragment: true,
        extensions: &NONE,
    });

    fn amended(source: &str) -> String {
        for_driver(source, &FRAGMENT).unwrap_or_else(|| source.to_owned())
    }

    fn lines(text: &str) -> usize {
        text.lines().count()
    }

    // ---- The conformance suite's cases (uninitialized-local-global-variables.html) ----

    #[test]
    fn a_local_is_initialized_where_it_is_declared() {
        let source = "precision mediump float;\nvoid main() {\n    vec2 uninit; // uninitialized\n    gl_FragColor = uninit.xyyy;\n}\n";
        let out = amended(source);
        assert!(
            out.contains("vec2 uninit = vec2(0.0); // uninitialized"),
            "{out}"
        );
        assert_eq!(
            lines(&out),
            lines(source),
            "the driver's line numbers are the source's"
        );
    }

    #[test]
    fn a_global_is_initialized_where_it_is_declared() {
        assert!(
            amended(
                "precision mediump float;\nvec4 uninit;\nvoid main() { gl_FragColor = uninit; }"
            )
            .contains("vec4 uninit = vec4(0.0);")
        );
        assert!(amended("float uninit;\nvoid main() {}").contains("float uninit = 0.0;"));
        assert!(amended("int uninit;\nvoid main() {}").contains("int uninit = 0;"));
    }

    #[test]
    fn a_local_int_and_bool_are_zero_and_false() {
        assert!(amended("void main() { int uninit; }").contains("int uninit = 0;"));
        assert!(amended("void main() { bool result; }").contains("bool result = false;"));
    }

    #[test]
    fn a_declarator_another_reads_is_zero_before_it_is_read() {
        let out = amended("void main() {\n    vec2 uninit, uninit2 = uninit;\n}");
        assert!(
            out.contains("vec2 uninit = vec2(0.0), uninit2 = uninit;"),
            "{out}"
        );
    }

    #[test]
    fn an_array_a_later_declarator_reads_is_zeroed_before_the_declaration_goes_on() {
        let out = amended("void main() {\n    vec2 uninit[2], uninit2 = uninit[0];\n}");
        assert!(
            out.contains(
                "vec2 uninit[2]; for (int _webgl_zi0 = 0; _webgl_zi0 < 2; ++_webgl_zi0) { uninit[_webgl_zi0] = vec2(0.0); } vec2 uninit2 = uninit[0];"
            ),
            "{out}"
        );
    }

    #[test]
    fn structures_named_and_nameless() {
        assert!(
            amended("struct S { vec4 v; };\nvoid main() { S uninit; }")
                .contains("S uninit = S(vec4(0.0));")
        );
        assert!(
            amended("struct S { vec4 v; };\nS uninit;\nvoid main() {}")
                .contains("S uninit = S(vec4(0.0));")
        );
        // A structure without a name has no constructor: its members are set.
        let local = amended(
            "void main() {\n    struct { vec4 v; } uninit;\n    gl_FragColor = uninit.v;\n}",
        );
        assert!(
            local.contains("struct { vec4 v; } uninit; uninit.v = vec4(0.0);"),
            "{local}"
        );
        let global =
            amended("struct { vec4 v; } uninit;\nvoid main() {\n    gl_FragColor = uninit.v;\n}");
        assert!(
            global.contains("void main() { uninit.v = vec4(0.0);"),
            "{global}"
        );
    }

    #[test]
    fn arrays_are_zeroed_element_by_element() {
        let local = amended("void main() {\n    bool uninit[16];\n}");
        assert!(
            local.contains("bool uninit[16]; for (int _webgl_zi0 = 0; _webgl_zi0 < 16; ++_webgl_zi0) { uninit[_webgl_zi0] = false; }"),
            "{local}"
        );
        let global = amended("bool uninit[16];\nvoid main() {\n}");
        assert!(
            global.contains("void main() { for (int _webgl_zi0 = 0; _webgl_zi0 < 16; ++_webgl_zi0) { uninit[_webgl_zi0] = false; }"),
            "{global}"
        );
        // ESSL 3.00 counts with length().
        let es3 = amended("#version 300 es\nvoid main() {\n    float a[4];\n}");
        assert!(es3.contains("_webgl_zi0 < a.length();"), "{es3}");
    }

    // ---- What content does with them ----

    #[test]
    fn a_global_a_function_adds_to_is_zero_in_the_shader_the_driver_gets() {
        // PlayCanvas's standard shader, cut down: dDiffuseLight is added to and never initialized.
        let source = "#version 300 es\nprecision highp float;\nuniform vec3 light_globalAmbient;\nvec3 dDiffuseLight;\nlayout(location = 0) out highp vec4 pcFragColor0;\n#define gl_FragColor pcFragColor0\nvoid addAmbient() { dDiffuseLight += light_globalAmbient; }\nvoid main(void) { addAmbient(); gl_FragColor.rgb = dDiffuseLight; }\n";
        let out = amended(source);
        assert!(out.contains("vec3 dDiffuseLight = vec3(0.0);"), "{out}");
        assert!(
            out.contains("uniform vec3 light_globalAmbient;\n"),
            "a uniform is the application's: {out}"
        );
        assert!(
            out.contains("out highp vec4 pcFragColor0;\n"),
            "an output is written, not read: {out}"
        );
    }

    #[test]
    fn what_macros_declare_is_read_through_them() {
        let source = "#define T vec3\n#define N 3\nT a;\nfloat b[N];\nvoid main() {}";
        let out = amended(source);
        assert!(out.contains("T a = vec3(0.0);"), "{out}");
        assert!(out.contains("_webgl_zi0 < 3;"), "{out}");
    }

    #[test]
    fn an_inactive_block_is_left_alone_and_an_active_one_is_read() {
        let source = "#ifdef GL_ES\nfloat a;\n#else\nfloat b;\n#endif\n#if defined(NOPE) || __VERSION__ >= 300\nfloat c;\n#endif\nvoid main() {}";
        let out = amended(source);
        assert!(out.contains("float a = 0.0;"));
        assert!(
            out.contains("float b;\n") && out.contains("float c;\n"),
            "{out}"
        );
    }

    #[test]
    fn a_webgl2_shader_lays_its_uniform_blocks_out_as_std140() {
        let source = "#version 300 es\nprecision highp float;\nuniform B { vec4 a; float f[3]; } b;\nout vec4 o;\nvoid main() { o = b.a; }\n";
        let out = amended(source);
        assert!(
            out.contains("\nlayout(std140) uniform; precision highp float;\n"),
            "{out}"
        );
        assert_eq!(
            lines(&out),
            lines(source),
            "the driver's line numbers are the source's"
        );
        // A layout the shader names is the shader's.
        let named =
            amended("#version 300 es\nlayout(shared) uniform B { vec4 a; } b;\nvoid main() {}\n");
        assert!(
            named.contains("layout(std140) uniform; layout(shared) uniform B"),
            "{named}"
        );
        // WebGL 1 has no uniform blocks.
        assert!(!amended("precision mediump float;\nvoid main() {}\n").contains("std140"));
    }

    #[test]
    fn a_declaration_a_macro_writes_is_left_to_the_macro() {
        let source = "#define DECLARE(t, n) t n;\nDECLARE(vec3, a)\nvoid main() {}";
        assert_eq!(for_driver(source, &FRAGMENT), None);
    }

    #[test]
    fn what_has_a_value_or_is_not_a_variable_is_left_alone() {
        for source in [
            "uniform vec3 u;\nattribute vec4 p;\nvarying vec2 v;\nconst float k = 1.0;\nvoid main() {}",
            "void main() { float x = 1.0; vec3 y = vec3(x); y.x = 2.0; vec3(1.0); }",
            "float f(float a, out float b);\nfloat f(float a, out float b) { b = a; return a; }\nvoid main() {}",
            "precision highp float;\ninvariant gl_Position;\nvoid main() {}",
            "void main() { for (int i = 0; i < 4; ++i) { if (i > 1) break; } }",
        ] {
            assert_eq!(for_driver(source, &FRAGMENT), None, "{source}");
        }
        // A GLSL ES 3.00 shader with nothing to initialize gains only the std140 default.
        let es3 = "#version 300 es\nin vec4 p;\nout vec4 o;\nlayout(std140) uniform B { mat4 m; } b;\nvoid main() { o = p; }";
        assert_eq!(
            for_driver(es3, &FRAGMENT).as_deref(),
            Some(
                es3.replacen("\nin vec4 p;", "\nlayout(std140) uniform; in vec4 p;", 1)
                    .as_str()
            )
        );
    }

    #[test]
    fn a_declaration_that_is_a_whole_body_takes_only_an_initializer() {
        // No statement can follow it in its scope: an array there is left as it is.
        let out = amended(
            "void main() { if (true) float x; for (int i; i < 2; ++i) {} if (true) float y[2]; }",
        );
        assert!(
            out.contains("float x = 0.0;") && out.contains("int i = 0;"),
            "{out}"
        );
        assert!(out.contains("float y[2]; }"), "{out}");
    }

    #[test]
    fn statements_of_every_kind_are_read() {
        let source = "void main() {\n  int n = 2;\n  switch (n) { case 1: float a; break; default: vec2 b; }\n  do { mat3 m; } while (false);\n  while (n > 0) { n--; uvec2 u; }\n}";
        let es3 = amended(&format!("#version 300 es\n{source}"));
        for expected in [
            "float a = 0.0;",
            "vec2 b = vec2(0.0);",
            "mat3 m = mat3(0.0);",
            "uvec2 u = uvec2(0u);",
        ] {
            assert!(es3.contains(expected), "{expected}: {es3}");
        }
    }

    #[test]
    fn a_shader_the_driver_will_refuse_is_left_to_the_driver() {
        for source in [
            "void main() { float x; ",
            "#if 1\nfloat x;\n",
            "#error no\nfloat x;\nvoid main() {}",
            "float x;\n#version 300 es\nvoid main() {}",
            "#define A(x, y) x\nfloat q = A(1);\nfloat x;\nvoid main() {}",
        ] {
            assert_eq!(for_driver(source, &FRAGMENT), None, "{source}");
        }
    }

    #[test]
    fn the_driver_s_extensions_are_its_macros() {
        let source = "#ifdef GL_OES_standard_derivatives\nfloat a;\n#endif\nvoid main() {}";
        assert_eq!(for_driver(source, &FRAGMENT), None);
        let extensions: HashSet<String> = ["GL_OES_standard_derivatives".to_owned()].into();
        let env = Environment {
            fragment: true,
            extensions: &extensions,
        };
        assert!(for_driver(source, &env).is_some_and(|out| out.contains("float a = 0.0;")));
    }

    #[test]
    fn only_a_webgl_compatible_driver_owns_shader_initialization() {
        let source =
            "precision mediump float; void main() { float a[2]; gl_FragColor = vec4(a[0]); }";
        for name in [
            "GL_ANGLE_webgl_compatibility",
            "GL_ANGLE_translated_shader_source",
            "GL_ANGLE_robust_resource_initialization",
            "GL_ANGLE_webgl_compatibility_extra",
        ] {
            let extensions = HashSet::from([name.to_owned()]);
            let env = Environment {
                fragment: true,
                extensions: &extensions,
            };
            assert_eq!(
                for_driver(source, &env).is_none(),
                name == "GL_ANGLE_webgl_compatibility",
                "{name}"
            );
        }
    }

    fn compile(gl: &glow::Context, kind: u32, source: &str) -> Result<glow::NativeShader, String> {
        use glow::HasContext;
        unsafe {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if gl.get_shader_compile_status(shader) {
                Ok(shader)
            } else {
                let log = gl.get_shader_info_log(shader);
                gl.delete_shader(shader);
                Err(log)
            }
        }
    }

    fn draw_pixel(gl: &glow::Context, version: i32, fragment: &str) -> [u8; 4] {
        use glow::HasContext;
        let vertex = if version == 2 {
            "attribute vec2 position; void main() { gl_Position = vec4(position, 0.0, 1.0); }"
        } else {
            "#version 300 es\nin vec2 position; void main() { gl_Position = vec4(position, 0.0, 1.0); }"
        };
        let vs = compile(gl, glow::VERTEX_SHADER, vertex).expect("vertex compiles");
        let fs = compile(gl, glow::FRAGMENT_SHADER, fragment).expect("fragment compiles");
        unsafe {
            let program = gl.create_program().unwrap();
            gl.attach_shader(program, vs);
            gl.attach_shader(program, fs);
            gl.bind_attrib_location(program, 0, "position");
            gl.link_program(program);
            assert!(
                gl.get_program_link_status(program),
                "{}",
                gl.get_program_info_log(program)
            );
            gl.use_program(Some(program));
            let vao = (version >= 3).then(|| gl.create_vertex_array().unwrap());
            if let Some(vao) = vao {
                gl.bind_vertex_array(Some(vao));
            }
            let buffer = gl.create_buffer().unwrap();
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
            let triangle: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck::cast_slice(&triangle),
                glow::STATIC_DRAW,
            );
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 0, 0);
            gl.viewport(0, 0, 3, 2);
            gl.clear_color(1.0, 0.0, 1.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            let mut pixel = [0u8; 4];
            gl.read_pixels(
                0,
                0,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixel)),
            );
            assert_eq!(gl.get_error(), glow::NO_ERROR);
            gl.delete_buffer(buffer);
            if let Some(vao) = vao {
                gl.delete_vertex_array(vao);
            }
            gl.delete_shader(vs);
            gl.delete_shader(fs);
            gl.delete_program(program);
            pixel
        }
    }

    #[test]
    #[ignore = "requires MIGO_TEST_ANGLE_EGL_LIBRARY and ANGLE/SwiftShader Vulkan"]
    fn angle_webgl_shader_validation_native() {
        use glow::HasContext;
        for version in [2, 3] {
            for webgl in [false, true] {
                let (_scope, gl) =
                    crate::backend::gl::readback_test_gl::native_angle_context(version, webgl);
                let env = Environment {
                    fragment: true,
                    extensions: gl.supported_extensions(),
                };
                assert_eq!(
                    env.extensions.contains("GL_ANGLE_webgl_compatibility"),
                    webgl
                );
                eprintln!(
                    "ES{version} webgl={webgl}: {} / {}",
                    unsafe { gl.get_parameter_string(glow::VERSION) },
                    unsafe { gl.get_parameter_string(glow::RENDERER) }
                );
                let (prefix, output) = if version == 2 {
                    ("precision highp float;\n", "gl_FragColor")
                } else {
                    (
                        "#version 300 es\nprecision highp float; out vec4 color;\n",
                        "color",
                    )
                };
                let valid = [
                    ("local", "", "float u;", "vec4(u, 0.0, 0.5, 1.0)"),
                    (
                        "local-array",
                        "",
                        "float a[2];",
                        "vec4(a[0], a[1], 0.5, 1.0)",
                    ),
                    (
                        "global-array",
                        "float a[2];\n",
                        "",
                        "vec4(a[0], a[1], 0.5, 1.0)",
                    ),
                    (
                        "struct-array",
                        "struct S { vec2 a[2]; };\n",
                        "S s;",
                        "vec4(s.a[0], 0.5, 1.0)",
                    ),
                    (
                        "macro-declaration",
                        "#define DECLARE(t,n) t n;\n",
                        "DECLARE(vec2,u)",
                        "vec4(u, 0.5, 1.0)",
                    ),
                    (
                        "initialized",
                        "",
                        "vec2 u = vec2(0.0);",
                        "vec4(u, 0.5, 1.0)",
                    ),
                ];
                for (name, global, local, value) in valid {
                    let source =
                        format!("{prefix}{global}void main() {{ {local} {output} = {value}; }}");
                    // First prove the mature WebGL path accepts and initializes the original.
                    if webgl {
                        assert_eq!(
                            draw_pixel(&gl, version, &source),
                            [0, 0, 128, 255],
                            "original ES{version} {name}"
                        );
                    }
                    let prepared = for_driver(&source, &env);
                    let driver = prepared.as_deref().unwrap_or(&source);
                    // Macro-created declarations are an existing native-path limitation.
                    if webgl || name != "macro-declaration" {
                        assert_eq!(
                            draw_pixel(&gl, version, driver),
                            [0, 0, 128, 255],
                            "prepared ES{version} webgl={webgl} {name}"
                        );
                    } else {
                        let shader = compile(&gl, glow::FRAGMENT_SHADER, driver).expect(
                            "native macro declaration compiles; initialization is not claimed",
                        );
                        unsafe { gl.delete_shader(shader) };
                    }
                    eprintln!(
                        "PASS ES{version} webgl={webgl} {name}{}",
                        if !webgl && name == "macro-declaration" {
                            " (compile only)"
                        } else {
                            ""
                        }
                    );
                }
                let reserved = format!(
                    "{prefix}void main() {{ float webgl_reserved = 1.0; {output} = vec4(webgl_reserved); }}"
                );
                let result = compile(&gl, glow::FRAGMENT_SHADER, &reserved);
                eprintln!(
                    "ES{version} webgl={webgl} reserved-prefix: {:?}",
                    result.as_ref().err()
                );
                assert_eq!(
                    result.is_err(),
                    webgl,
                    "reserved prefix ES{version}: {result:?}"
                );
                if let Ok(shader) = result {
                    unsafe {
                        gl.delete_shader(shader);
                    }
                }
                if version == 2 {
                    let dynamic_loop = "precision highp float; uniform int count; void main() { float sum = 0.0; for (int i = 0; i < count; ++i) { sum += 0.1; } gl_FragColor = vec4(sum); }";
                    let result = compile(&gl, glow::FRAGMENT_SHADER, dynamic_loop);
                    eprintln!(
                        "ES{version} webgl={webgl} dynamic-loop: {:?}",
                        result.as_ref().err()
                    );
                    assert_eq!(result.is_err(), webgl, "nonconstant loop: {result:?}");
                    if let Ok(shader) = result {
                        unsafe {
                            gl.delete_shader(shader);
                        }
                    }
                }
                let invalid = format!("{prefix}void main() {{ {output} = unknown_identifier; }}");
                assert!(compile(&gl, glow::FRAGMENT_SHADER, &invalid).is_err());
                eprintln!("PASS ES{version} webgl={webgl} validation");
            }
        }
    }
}
