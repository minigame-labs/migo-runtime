//! WebGL's shading language on a driver that is not a browser's.
//!
//! A browser hands a driver what ANGLE's translator made of the content's shader; the renderer hands it the shader
//! itself. Where the two languages differ in what a driver must be told, the source is amended before the driver
//! sees it -- and only then: `getShaderSource` still answers what content set.

mod lexer;
mod preprocess;
mod zero_init;

pub(crate) use preprocess::Environment;

/// The source a driver should compile for the WebGL shader `source`: every variable it leaves without a value
/// initialized to zero, as WebGL requires and a driver need not do (see `zero_init`). `None` when the driver can take
/// `source` as it is -- nothing is missing, or the shader is one the driver will refuse, which it then reports itself.
pub(crate) fn for_driver(source: &str, env: &Environment) -> Option<String> {
    let preprocessed = preprocess::preprocess(source, env).ok()?;
    let edits = zero_init::edits(&preprocessed.tokens, preprocessed.version).ok()?;
    if edits.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(source.len() + edits.iter().map(|e| e.text.len()).sum::<usize>());
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
        assert!(out.contains("vec2 uninit = vec2(0.0); // uninitialized"), "{out}");
        assert_eq!(lines(&out), lines(source), "the driver's line numbers are the source's");
    }

    #[test]
    fn a_global_is_initialized_where_it_is_declared() {
        assert!(amended("precision mediump float;\nvec4 uninit;\nvoid main() { gl_FragColor = uninit; }")
            .contains("vec4 uninit = vec4(0.0);"));
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
        assert!(out.contains("vec2 uninit = vec2(0.0), uninit2 = uninit;"), "{out}");
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
        assert!(amended("struct S { vec4 v; };\nvoid main() { S uninit; }").contains("S uninit = S(vec4(0.0));"));
        assert!(amended("struct S { vec4 v; };\nS uninit;\nvoid main() {}").contains("S uninit = S(vec4(0.0));"));
        // A structure without a name has no constructor: its members are set.
        let local = amended("void main() {\n    struct { vec4 v; } uninit;\n    gl_FragColor = uninit.v;\n}");
        assert!(local.contains("struct { vec4 v; } uninit; uninit.v = vec4(0.0);"), "{local}");
        let global = amended("struct { vec4 v; } uninit;\nvoid main() {\n    gl_FragColor = uninit.v;\n}");
        assert!(global.contains("void main() { uninit.v = vec4(0.0);"), "{global}");
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
        assert!(out.contains("uniform vec3 light_globalAmbient;\n"), "a uniform is the application's: {out}");
        assert!(out.contains("out highp vec4 pcFragColor0;\n"), "an output is written, not read: {out}");
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
        assert!(out.contains("float b;\n") && out.contains("float c;\n"), "{out}");
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
            "#version 300 es\nin vec4 p;\nout vec4 o;\nlayout(std140) uniform B { mat4 m; } b;\nvoid main() { o = p; }",
            "void main() { float x = 1.0; vec3 y = vec3(x); y.x = 2.0; vec3(1.0); }",
            "float f(float a, out float b);\nfloat f(float a, out float b) { b = a; return a; }\nvoid main() {}",
            "precision highp float;\ninvariant gl_Position;\nvoid main() {}",
            "void main() { for (int i = 0; i < 4; ++i) { if (i > 1) break; } }",
        ] {
            assert_eq!(for_driver(source, &FRAGMENT), None, "{source}");
        }
    }

    #[test]
    fn a_declaration_that_is_a_whole_body_takes_only_an_initializer() {
        // No statement can follow it in its scope: an array there is left as it is.
        let out = amended("void main() { if (true) float x; for (int i; i < 2; ++i) {} if (true) float y[2]; }");
        assert!(out.contains("float x = 0.0;") && out.contains("int i = 0;"), "{out}");
        assert!(out.contains("float y[2]; }"), "{out}");
    }

    #[test]
    fn statements_of_every_kind_are_read() {
        let source = "void main() {\n  int n = 2;\n  switch (n) { case 1: float a; break; default: vec2 b; }\n  do { mat3 m; } while (false);\n  while (n > 0) { n--; uvec2 u; }\n}";
        let es3 = amended(&format!("#version 300 es\n{source}"));
        for expected in ["float a = 0.0;", "vec2 b = vec2(0.0);", "mat3 m = mat3(0.0);", "uvec2 u = uvec2(0u);"] {
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
}

