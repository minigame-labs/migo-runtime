//! The order a program's active uniforms are listed in.
//!
//! `getActiveUniform(program, i)` walks a list whose order neither GLES nor WebGL fixes, and a driver lists it as it
//! likes: Mali puts a fragment shader's own `uSampler` ahead of the `environmentMapDiffused` and
//! `environmentMapSpecular` a prepended block of it declares first. Content is written against what browsers list --
//! p5 hands out texture units in this order, and binds the default texture it makes on its first draw to whichever
//! unit is active, which is the unit of the first sampler listed. Where that is `uSampler` the draw samples the default
//! texture instead of the image: p5's `filter()` comes back transparent on a Mate 30 and right in every browser.
//!
//! What every browser does, and what content can build on, is ANGLE's linker: the default block's samplers come after
//! its values, in the order the shaders declare them, the vertex shader's ahead of the fragment shader's own, and the
//! members of uniform blocks come last. So the runtime lists them so on every driver, and the rest the same way, so
//! that one program lists its uniforms in one order on every device: the values too in declaration order (where
//! browsers differ -- ANGLE's Metal backend sorts them by size -- and no content can rely on one), a structure's members
//! in the order its definition gives them, element by element for an array of structures. Only the order changes --
//! the uniforms are the driver's, and every index the WebGL API takes or answers is translated ([`UniformOrder`]). A
//! context whose driver is ANGLE's WebGL-compatible compiler lists them as a browser does already and is left as it is.

use std::collections::HashMap;

use super::preprocess::PpToken;

/// What a shader declares about its uniforms: the variables of the default block, the uniform blocks and the
/// structures their types name, in the order of the shader's text.
#[derive(Debug, Default)]
pub(crate) struct Declarations {
    values: Vec<Variable>,
    blocks: Vec<Block>,
    /// Every structure the shader defines, its members in order; [`Variable::structure`] indexes it.
    structures: Vec<Vec<Variable>>,
}

#[derive(Debug)]
struct Variable {
    name: String,
    /// The structure it is (or is an array of), or `None` for a basic type.
    structure: Option<usize>,
}

#[derive(Debug)]
struct Block {
    name: String,
    /// The instance name: the driver names a member `Block.member` when there is one, `member` when there is not.
    instance: bool,
    members: Vec<Variable>,
}

/// Qualifiers a declaration may carry ahead of its type. `layout(...)` is read separately.
const QUALIFIERS: &[&str] = &[
    "uniform",
    "const",
    "in",
    "out",
    "inout",
    "attribute",
    "varying",
    "invariant",
    "flat",
    "smooth",
    "centroid",
    "highp",
    "mediump",
    "lowp",
];

impl Declarations {
    /// The declarations of a preprocessed shader: the code the driver compiles, every macro expanded. Code that does
    /// not read as GLSL is passed over to the next declaration; the driver reports it.
    pub(super) fn read(tokens: &[PpToken<'_>]) -> Self {
        let mut reader = Reader {
            t: tokens,
            i: 0,
            named: HashMap::new(),
            out: Declarations::default(),
        };
        while reader.i < reader.t.len() {
            let before = reader.i;
            reader.external_declaration();
            if reader.i == before {
                reader.i += 1;
            }
        }
        reader.out
    }
}

struct Reader<'t, 'a> {
    t: &'t [PpToken<'a>],
    i: usize,
    /// Structure names, to their index in `out.structures`.
    named: HashMap<String, usize>,
    out: Declarations,
}

impl Reader<'_, '_> {
    fn here(&self) -> &str {
        self.t.get(self.i).map_or("", |token| token.text.as_ref())
    }

    fn ident_here(&self) -> Option<String> {
        self.t
            .get(self.i)
            .filter(|token| token.is_ident())
            .map(|token| token.text.to_string())
    }

    /// Past a bracketed group the cursor is on the opening of.
    fn skip_balanced(&mut self) {
        let mut depth = 0usize;
        while let Some(token) = self.t.get(self.i) {
            self.i += 1;
            match token.text.as_ref() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    /// Past the rest of a declaration: to and past its `;`, or past the body of a function definition.
    fn skip_statement(&mut self) {
        while let Some(token) = self.t.get(self.i) {
            match token.text.as_ref() {
                ";" => {
                    self.i += 1;
                    return;
                }
                "{" => {
                    self.skip_balanced();
                    return;
                }
                "(" | "[" => self.skip_balanced(),
                _ => self.i += 1,
            }
        }
    }

    /// The qualifiers ahead of a type, `layout(...)` among them: whether `uniform` is one.
    fn qualifiers(&mut self) -> bool {
        let mut uniform = false;
        loop {
            match self.here() {
                "layout" => {
                    self.i += 1;
                    if self.here() == "(" {
                        self.skip_balanced();
                    }
                }
                word if QUALIFIERS.contains(&word) => {
                    uniform |= word == "uniform";
                    self.i += 1;
                }
                _ => return uniform,
            }
        }
    }

    fn skip_dims(&mut self) {
        while self.here() == "[" {
            self.skip_balanced();
        }
    }

    /// A type: a structure defined here, or a type's name, and any array dimensions after it. The structure it is,
    /// if it is one; `Err` when there is no type here.
    fn type_specifier(&mut self) -> Result<Option<usize>, ()> {
        let structure = if self.here() == "struct" {
            self.i += 1;
            Some(self.structure_definition()?)
        } else {
            let name = self.ident_here().ok_or(())?;
            self.i += 1;
            self.named.get(&name).copied()
        };
        self.skip_dims();
        Ok(structure)
    }

    /// `[Name] { members }`, after `struct`: the new structure's index.
    fn structure_definition(&mut self) -> Result<usize, ()> {
        let name = self.ident_here();
        if name.is_some() {
            self.i += 1;
        }
        let members = self.members()?;
        let index = self.out.structures.len();
        self.out.structures.push(members);
        if let Some(name) = name {
            self.named.insert(name, index);
        }
        Ok(index)
    }

    /// `{ declarations }` of a structure or a block: its members, in order.
    fn members(&mut self) -> Result<Vec<Variable>, ()> {
        if self.here() != "{" {
            return Err(());
        }
        self.i += 1;
        let mut members = Vec::new();
        while self.here() != "}" {
            if self.i >= self.t.len() {
                return Err(());
            }
            self.qualifiers();
            let structure = self.type_specifier()?;
            for name in self.declarators()? {
                members.push(Variable { name, structure });
            }
        }
        self.i += 1;
        Ok(members)
    }

    /// `name[dims] [= value], ... ;`: the names, the cursor past the `;`.
    fn declarators(&mut self) -> Result<Vec<String>, ()> {
        let mut names = Vec::new();
        if self.here() == ";" {
            self.i += 1;
            return Ok(names);
        }
        loop {
            names.push(self.ident_here().ok_or(())?);
            self.i += 1;
            self.skip_dims();
            if self.here() == "=" {
                while !matches!(self.here(), "," | ";" | "") {
                    if matches!(self.here(), "(" | "[" | "{") {
                        self.skip_balanced();
                    } else {
                        self.i += 1;
                    }
                }
            }
            match self.here() {
                "," => self.i += 1,
                ";" => {
                    self.i += 1;
                    return Ok(names);
                }
                _ => return Err(()),
            }
        }
    }

    fn external_declaration(&mut self) {
        if self.here() == "precision" {
            self.skip_statement();
            return;
        }
        let start = self.i;
        if self.read_declaration().is_err() {
            self.i = start;
            self.skip_statement();
        }
    }

    fn read_declaration(&mut self) -> Result<(), ()> {
        let uniform = self.qualifiers();
        // `uniform Name { ... }` is a block; `uniform struct { ... }` a structure, whose keyword reads as a name.
        if uniform
            && let Some(name) = self.ident_here().filter(|name| name != "struct")
            && self.t.get(self.i + 1).is_some_and(|next| next.is("{"))
        {
            self.i += 1;
            let members = self.members()?;
            let instance = self.ident_here().is_some();
            if instance {
                self.i += 1;
                self.skip_dims();
            }
            if self.here() != ";" {
                return Err(());
            }
            self.i += 1;
            self.out.blocks.push(Block {
                name,
                instance,
                members,
            });
            return Ok(());
        }
        let structure = self.type_specifier()?;
        // A function: its prototype, or its definition and body.
        if self.ident_here().is_some() && self.t.get(self.i + 1).is_some_and(|next| next.is("(")) {
            self.i += 1;
            self.skip_balanced();
            if self.here() == "{" {
                self.skip_balanced();
            } else {
                self.skip_statement();
            }
            return Ok(());
        }
        let names = self.declarators()?;
        if uniform {
            self.out
                .values
                .extend(names.into_iter().map(|name| Variable { name, structure }));
        }
        Ok(())
    }
}

/// An active uniform as the driver lists it.
pub(crate) struct Active<'n> {
    pub name: &'n str,
    /// Its type is a sampler's.
    pub sampler: bool,
    /// It is a member of a uniform block (its `UNIFORM_BLOCK_INDEX` is not -1).
    pub in_block: bool,
}

/// A program's active uniforms in WebGL's order: each WebGL index's driver index, and each driver index's WebGL one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UniformOrder {
    to_driver: Box<[u32]>,
    from_driver: Box<[u32]>,
}

impl UniformOrder {
    /// The driver's index of the uniform WebGL lists at `index`, or `None` past the last.
    pub(crate) fn driver(&self, index: u32) -> Option<u32> {
        self.to_driver.get(index as usize).copied()
    }

    /// WebGL's index of the uniform the driver lists at `index`, or `None` past the last.
    pub(crate) fn webgl(&self, index: u32) -> Option<u32> {
        self.from_driver.get(index as usize).copied()
    }
}

/// Where a uniform sorts: its group, its declaration's place, the members and elements its name goes through, and,
/// last, the driver's index -- which only decides between uniforms the declarations do not place.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    /// The default block's values, its samplers, the members of uniform blocks.
    group: u8,
    root: u32,
    path: Vec<u32>,
    driver: u32,
}

/// Neither shader declares it as it is named: after everything that is placed.
const UNPLACED: u32 = u32::MAX;

/// A part of a uniform's name after its first identifier.
enum Segment<'n> {
    Element(u32),
    Member(&'n str),
}

/// `s[1].v[0]` as `s`, then element 1, member `v`, element 0; `None` for a name that is not one.
fn segments(name: &str) -> Option<(&str, Vec<Segment<'_>>)> {
    let end = name.find(['[', '.']).unwrap_or(name.len());
    let (root, mut rest) = name.split_at(end);
    let mut out = Vec::new();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('[') {
            let close = after.find(']')?;
            out.push(Segment::Element(after[..close].parse().ok()?));
            rest = &after[close + 1..];
        } else {
            let after = rest.strip_prefix('.')?;
            let end = after.find(['[', '.']).unwrap_or(after.len());
            out.push(Segment::Member(&after[..end]));
            rest = &after[end..];
        }
    }
    (!root.is_empty()).then_some((root, out))
}

/// The members and elements `segments` go through, from a variable of `structure` in `shader`, appended to `path`.
/// A member the structure does not have is placed after every one it does, and ends the walk.
fn walk(
    shader: &Declarations,
    mut structure: Option<usize>,
    segments: &[Segment<'_>],
    path: &mut Vec<u32>,
) {
    for segment in segments {
        match segment {
            Segment::Element(index) => path.push(*index),
            Segment::Member(name) => {
                let member = structure.and_then(|s| {
                    shader.structures[s]
                        .iter()
                        .position(|member| member.name == *name)
                        .map(|at| (at, shader.structures[s][at].structure))
                });
                match member {
                    Some((at, inner)) => {
                        path.push(at as u32);
                        structure = inner;
                    }
                    None => {
                        path.push(UNPLACED);
                        return;
                    }
                }
            }
        }
    }
}

/// The order WebGL lists `active` in, read from the program's two shaders (see the module documentation).
pub(crate) fn order(
    vertex: &Declarations,
    fragment: &Declarations,
    active: &[Active<'_>],
) -> UniformOrder {
    // Each name's first declaration: the vertex shader's, then the fragment shader's own.
    let mut values: HashMap<&str, (u32, &Declarations, &Variable)> = HashMap::new();
    let mut blocks: HashMap<&str, (u32, &Declarations, &Block)> = HashMap::new();
    for shader in [vertex, fragment] {
        for variable in &shader.values {
            let next = values.len() as u32;
            values
                .entry(variable.name.as_str())
                .or_insert((next, shader, variable));
        }
        for block in &shader.blocks {
            let next = blocks.len() as u32;
            blocks
                .entry(block.name.as_str())
                .or_insert((next, shader, block));
        }
    }
    // A member of a block with no instance name is named by itself.
    let mut bare: HashMap<&str, (u32, u32, &Declarations, &Variable)> = HashMap::new();
    let mut ordered: Vec<_> = blocks.values().copied().collect();
    ordered.sort_by_key(|(at, _, _)| *at);
    for (at, shader, block) in ordered {
        if block.instance {
            continue;
        }
        for (member, variable) in block.members.iter().enumerate() {
            bare.entry(variable.name.as_str())
                .or_insert((at, member as u32, shader, variable));
        }
    }

    let key = |driver: u32, uniform: &Active<'_>| -> Key {
        let group = if uniform.in_block {
            2
        } else if uniform.sampler {
            1
        } else {
            0
        };
        let mut path = Vec::new();
        let root = match segments(uniform.name) {
            None => UNPLACED,
            Some((root, rest)) if uniform.in_block => {
                let instanced = blocks.get(root).filter(|(_, _, block)| block.instance);
                if let Some(&(at, shader, block)) = instanced {
                    match rest.split_first() {
                        Some((Segment::Member(name), rest)) => {
                            match block.members.iter().position(|m| m.name == *name) {
                                Some(member) => {
                                    path.push(member as u32);
                                    walk(shader, block.members[member].structure, rest, &mut path);
                                }
                                None => path.push(UNPLACED),
                            }
                            at
                        }
                        _ => UNPLACED,
                    }
                } else if let Some(&(at, member, shader, variable)) = bare.get(root) {
                    path.push(member);
                    walk(shader, variable.structure, &rest, &mut path);
                    at
                } else {
                    UNPLACED
                }
            }
            Some((root, rest)) => match values.get(root) {
                Some(&(at, shader, variable)) => {
                    walk(shader, variable.structure, &rest, &mut path);
                    at
                }
                None => UNPLACED,
            },
        };
        Key {
            group,
            root,
            path,
            driver,
        }
    };

    let mut keyed: Vec<Key> = active
        .iter()
        .enumerate()
        .map(|(driver, uniform)| key(driver as u32, uniform))
        .collect();
    keyed.sort_unstable();
    let to_driver: Box<[u32]> = keyed.iter().map(|key| key.driver).collect();
    let mut from_driver = vec![0u32; to_driver.len()].into_boxed_slice();
    for (webgl, &driver) in to_driver.iter().enumerate() {
        from_driver[driver as usize] = webgl as u32;
    }
    UniformOrder {
        to_driver,
        from_driver,
    }
}

#[cfg(test)]
mod tests {
    use super::super::preprocess::{Environment, preprocess};
    use super::*;
    use std::collections::HashSet;

    fn declarations(source: &str, fragment: bool) -> Declarations {
        let extensions = HashSet::new();
        let env = Environment {
            fragment,
            extensions: &extensions,
        };
        Declarations::read(&preprocess(source, &env).expect("readable").tokens)
    }

    fn value(name: &str) -> Active<'_> {
        Active {
            name,
            sampler: false,
            in_block: false,
        }
    }

    fn sampler(name: &str) -> Active<'_> {
        Active {
            name,
            sampler: true,
            in_block: false,
        }
    }

    fn member(name: &str) -> Active<'_> {
        Active {
            name,
            sampler: false,
            in_block: true,
        }
    }

    /// The names in the order WebGL lists them.
    fn listed<'n>(vertex: &str, fragment: &str, active: &[Active<'n>]) -> Vec<&'n str> {
        let order = order(
            &declarations(vertex, false),
            &declarations(fragment, true),
            active,
        );
        (0..active.len() as u32)
            .map(|webgl| active[order.driver(webgl).unwrap() as usize].name)
            .collect()
    }

    const P5_VERTEX: &str = "precision highp float;\n\
        attribute vec3 aPosition;\n\
        uniform mat4 uModelViewMatrix;\n\
        uniform mat4 uProjectionMatrix;\n\
        void main() { gl_Position = uProjectionMatrix * uModelViewMatrix * vec4(aPosition, 1.0); }\n";

    /// p5's light shader, cut down: a prepended block declares the environment maps, the shader's own text the image
    /// it draws. Mali lists `uSampler` first.
    const P5_FRAGMENT: &str = "precision highp float;\n\
        uniform bool uUseImageLight;\n\
        uniform sampler2D environmentMapDiffused;\n\
        uniform sampler2D environmentMapSpecular;\n\
        // the shader's own part\n\
        uniform vec4 uTint;\n\
        uniform sampler2D uSampler;\n\
        void main() { gl_FragColor = texture2D(uSampler, vec2(0.0)) * uTint; }\n";

    #[test]
    fn samplers_come_after_values_in_the_order_the_shader_declares_them() {
        let mali = [
            sampler("uSampler"),
            sampler("environmentMapSpecular"),
            value("uTint"),
            sampler("environmentMapDiffused"),
            value("uProjectionMatrix"),
            value("uUseImageLight"),
            value("uModelViewMatrix"),
        ];
        assert_eq!(
            listed(P5_VERTEX, P5_FRAGMENT, &mali),
            [
                "uModelViewMatrix",
                "uProjectionMatrix",
                "uUseImageLight",
                "uTint",
                "environmentMapDiffused",
                "environmentMapSpecular",
                "uSampler",
            ]
        );
    }

    #[test]
    fn the_order_is_the_same_whatever_order_the_driver_lists_them_in() {
        let names = [
            "uSampler",
            "environmentMapSpecular",
            "environmentMapDiffused",
            "uTint",
        ];
        let expected = [
            "uTint",
            "environmentMapDiffused",
            "environmentMapSpecular",
            "uSampler",
        ];
        let permutations = [[0, 1, 2, 3], [3, 2, 1, 0], [2, 0, 3, 1], [1, 3, 0, 2]];
        for permutation in permutations {
            let active: Vec<Active<'_>> = permutation
                .iter()
                .map(|&i| {
                    if names[i] == "uTint" {
                        value(names[i])
                    } else {
                        sampler(names[i])
                    }
                })
                .collect();
            assert_eq!(
                listed(P5_VERTEX, P5_FRAGMENT, &active),
                expected,
                "{permutation:?}"
            );
        }
    }

    #[test]
    fn the_index_maps_go_both_ways() {
        let active = [sampler("b"), value("a")];
        let order = order(
            &declarations(
                "uniform float a; uniform sampler2D b; void main() {}",
                false,
            ),
            &Declarations::default(),
            &active,
        );
        assert_eq!(
            (order.driver(0), order.driver(1), order.driver(2)),
            (Some(1), Some(0), None)
        );
        assert_eq!(
            (order.webgl(0), order.webgl(1), order.webgl(2)),
            (Some(1), Some(0), None)
        );
    }

    #[test]
    fn the_vertex_shader_comes_first_and_a_name_both_declare_is_placed_by_it() {
        let vertex = "uniform float shared; uniform float v; void main() {}";
        let fragment =
            "precision mediump float; uniform float f; uniform float shared; void main() {}";
        let active = [value("f"), value("shared"), value("v")];
        assert_eq!(listed(vertex, fragment, &active), ["shared", "v", "f"]);
    }

    #[test]
    fn a_structure_is_listed_member_by_member_and_an_array_of_them_element_by_element() {
        let vertex = "struct Light { vec3 color; float power; vec3 position; };\n\
            uniform Light lights[2];\n\
            uniform float after;\n\
            void main() {}";
        let active = [
            value("after"),
            value("lights[1].position"),
            value("lights[0].power"),
            value("lights[1].color"),
            value("lights[0].position"),
            value("lights[1].power"),
            value("lights[0].color"),
        ];
        assert_eq!(
            listed(vertex, "void main() {}", &active),
            [
                "lights[0].color",
                "lights[0].power",
                "lights[0].position",
                "lights[1].color",
                "lights[1].power",
                "lights[1].position",
                "after",
            ]
        );
    }

    #[test]
    fn a_nested_or_inline_structure_is_followed_through_its_members() {
        let vertex = "struct Inner { float b; float a; };\n\
            struct Outer { Inner first; float[2] arr; Inner second; };\n\
            uniform Outer o;\n\
            uniform struct { float y; float x; } anon;\n\
            void main() {}";
        let active = [
            value("anon.x"),
            value("o.second.a"),
            value("o.arr[0]"),
            value("anon.y"),
            value("o.first.a"),
            value("o.second.b"),
            value("o.first.b"),
        ];
        assert_eq!(
            listed(vertex, "void main() {}", &active),
            [
                "o.first.b",
                "o.first.a",
                "o.arr[0]",
                "o.second.b",
                "o.second.a",
                "anon.y",
                "anon.x"
            ]
        );
    }

    #[test]
    fn a_declaration_of_several_names_lists_them_in_its_order() {
        let vertex = "uniform highp float c, a[3], b;\nvoid main() {}";
        let active = [value("b"), value("a[0]"), value("c")];
        assert_eq!(
            listed(vertex, "void main() {}", &active),
            ["c", "a[0]", "b"]
        );
    }

    #[test]
    fn block_members_come_last_by_block_and_by_member() {
        let vertex = "#version 300 es\n\
            layout(std140) uniform Camera { mat4 view; mat4 projection; } camera;\n\
            uniform Bare { vec4 second; vec4 first; };\n\
            uniform sampler2D tex;\n\
            uniform float value;\n\
            void main() {}";
        let active = [
            member("first"),
            sampler("tex"),
            member("Camera.projection"),
            value("value"),
            member("second"),
            member("Camera.view"),
        ];
        assert_eq!(
            listed(vertex, "#version 300 es\nvoid main() {}", &active),
            [
                "value",
                "tex",
                "Camera.view",
                "Camera.projection",
                "second",
                "first"
            ]
        );
    }

    #[test]
    fn what_a_macro_or_an_inactive_branch_does_is_what_the_driver_compiles() {
        let vertex = "#define DECLARE(n) uniform float n;\n\
            #ifdef NOT_DEFINED\n\
            uniform float first;\n\
            #endif\n\
            uniform float second;\n\
            DECLARE(third)\n\
            uniform float first;\n\
            void main() {}";
        let active = [value("first"), value("third"), value("second")];
        assert_eq!(
            listed(vertex, "void main() {}", &active),
            ["second", "third", "first"]
        );
    }

    #[test]
    fn functions_locals_and_other_globals_are_passed_over() {
        let vertex = "precision mediump float;\n\
            const float k = 2.0;\n\
            vec3 helper(vec3 v) { float uniformish = 1.0; return v * k; }\n\
            float proto(float);\n\
            uniform float late;\n\
            attribute vec2 p;\n\
            varying vec2 uv;\n\
            uniform float later;\n\
            void main() { gl_Position = vec4(helper(vec3(p, late)), later); }";
        let active = [value("later"), value("late")];
        assert_eq!(listed(vertex, "void main() {}", &active), ["late", "later"]);
    }

    #[test]
    fn a_name_neither_shader_declares_follows_in_the_drivers_order() {
        let vertex = "uniform float known; void main() {}";
        let active = [value("mystery2"), value("known"), value("mystery1")];
        assert_eq!(
            listed(vertex, "void main() {}", &active),
            ["known", "mystery2", "mystery1"]
        );
    }

    #[test]
    fn code_that_does_not_read_as_glsl_does_not_stop_the_reader() {
        let vertex = "uniform float a; @@@ garbage ; uniform float b; void main() {}";
        let active = [value("b"), value("a")];
        assert_eq!(listed(vertex, "void main() {}", &active), ["a", "b"]);
        // Unbalanced to the end: nothing is lost before it, and the reader returns.
        let truncated = declarations("uniform float a; uniform Block { float x;", false);
        assert_eq!(truncated.values.len(), 1);
    }

    #[test]
    fn names_split_into_their_segments() {
        let (root, rest) = segments("s[1].v[3]").unwrap();
        assert_eq!(root, "s");
        assert!(matches!(
            rest.as_slice(),
            [
                Segment::Element(1),
                Segment::Member("v"),
                Segment::Element(3)
            ]
        ));
        assert!(segments("a[x]").is_none());
        assert!(segments("").is_none());
        assert!(segments("a[").is_none());
    }
}
