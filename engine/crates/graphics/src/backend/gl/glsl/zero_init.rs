//! Where a shader declares a variable without a value, and the text that gives it zero.
//!
//! GLSL ES leaves such a variable undefined. WebGL does not: the conformance suite's
//! `uninitialized-local-global-variables` requires every local and global a shader leaves without an initializer to
//! read as zero, and content is written against that -- PlayCanvas's standard shader adds to a global it never
//! initializes. A browser gets it from ANGLE's translator. A driver given the source as it stands may take the
//! undefined value at its word: Mesa folds everything computed from it away, the uniforms it was mixed with become
//! inactive, and the fragment is black.
//!
//! So the text is inserted where it is missing, in the source the driver gets, on the line it belongs to so the
//! driver's messages keep their line numbers:
//!
//! - a scalar, vector, matrix or named structure gets an initializer, `= vec3(0.0)`, where it is declared -- before
//!   the next declarator in the same declaration can read it;
//! - an array, or a structure with no name or with an array in it, which no initializer of both languages can write,
//!   is assigned zero element by element: a local right after its declaration (split from a declarator after it,
//!   which may read it), a global at the start of `main`. The loops count with `_webgl_`-prefixed names, which WebGL
//!   reserves for the implementation.
//!
//! What cannot be reached in the source -- a declaration a macro wrote -- is left as it is.

use std::collections::HashMap;

use super::preprocess::{PpToken, Unreadable};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scalar {
    Float,
    Int,
    Uint,
    Bool,
}

impl Scalar {
    fn zero(self) -> &'static str {
        match self {
            Scalar::Float => "0.0",
            Scalar::Int => "0",
            Scalar::Uint => "0u",
            Scalar::Bool => "false",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Type {
    Scalar(Scalar),
    Vector(Scalar, u8),
    /// Columns, rows.
    Matrix(u8, u8),
    /// An index into the structures.
    Struct(usize),
    /// Samplers and `void`: nothing a variable outside a uniform or a parameter has.
    Opaque,
}

fn builtin(name: &str) -> Option<Type> {
    let vector = |prefix: &str, scalar| {
        name.strip_prefix(prefix)
            .and_then(|n| n.parse::<u8>().ok())
            .filter(|n| (2..=4).contains(n))
            .map(|n| Type::Vector(scalar, n))
    };
    Some(match name {
        "float" => Type::Scalar(Scalar::Float),
        "int" => Type::Scalar(Scalar::Int),
        "uint" => Type::Scalar(Scalar::Uint),
        "bool" => Type::Scalar(Scalar::Bool),
        "void" => Type::Opaque,
        _ if name.starts_with("sampler")
            || name.starts_with("isampler")
            || name.starts_with("usampler") =>
        {
            Type::Opaque
        }
        _ => {
            if let Some(v) = vector("vec", Scalar::Float)
                .or_else(|| vector("ivec", Scalar::Int))
                .or_else(|| vector("uvec", Scalar::Uint))
                .or_else(|| vector("bvec", Scalar::Bool))
            {
                return Some(v);
            }
            let size = name.strip_prefix("mat")?;
            let (columns, rows) = match size.split_once('x') {
                Some((c, r)) => (c.parse().ok()?, r.parse().ok()?),
                None => {
                    let n = size.parse().ok()?;
                    (n, n)
                }
            };
            if !(2..=4).contains(&columns) || !(2..=4).contains(&rows) {
                return None;
            }
            Type::Matrix(columns, rows)
        }
    })
}

/// An array dimension: its size as the driver reads it.
#[derive(Clone, Debug)]
struct Dim(String);

#[derive(Debug)]
struct Field {
    name: String,
    ty: Type,
    dims: Vec<Dim>,
}

#[derive(Debug)]
struct Structure {
    name: Option<String>,
    fields: Vec<Field>,
}

const STORAGE: [&str; 9] = [
    "const",
    "uniform",
    "in",
    "out",
    "inout",
    "attribute",
    "varying",
    "buffer",
    "shared",
];
const OTHER_QUALIFIERS: [&str; 12] = [
    "centroid",
    "flat",
    "smooth",
    "noperspective",
    "invariant",
    "precise",
    "patch",
    "sample",
    "highp",
    "mediump",
    "lowp",
    "coherent",
];

/// One declarator of a declaration.
struct Declarator {
    name: String,
    dims: Vec<Dim>,
    /// The token that ends it, before any initializer.
    end: usize,
    initialized: bool,
    /// The comma after it, when another declarator follows.
    comma: Option<usize>,
}

struct Declaration {
    /// Its first token, and the last before its first declarator's name.
    first: usize,
    before_name: usize,
    ty: Type,
    /// Declares a structure it names: repeated, the declaration names it rather than defining it again.
    defines: Option<String>,
    semicolon: usize,
    declarators: Vec<Declarator>,
    /// No storage qualifier: an ordinary variable.
    ordinary: bool,
}

/// Replace `start..end` in the source with `text`.
#[derive(Debug)]
pub(super) struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

pub(super) fn edits(tokens: &[PpToken<'_>], version: u32) -> Result<Vec<Edit>, Unreadable> {
    let mut reader = Reader {
        t: tokens,
        i: 0,
        structures: Vec::new(),
        names: HashMap::new(),
        es3: version >= 300,
        edits: Vec::new(),
        globals: Vec::new(),
        main: None,
    };
    reader.translation_unit()?;
    let mut edits = std::mem::take(&mut reader.edits);
    let globals = std::mem::take(&mut reader.globals);
    let main = reader.main;
    let mut prologue = Vec::new();
    let w = Writer {
        t: tokens,
        structures: &reader.structures,
        es3: reader.es3,
    };
    for declaration in globals.iter().filter(|d| d.ordinary) {
        for declarator in declaration.declarators.iter().filter(|d| !d.initialized) {
            if declarator.dims.is_empty()
                && let Some(zero) = w.zero(declaration.ty)
            {
                w.insert_after(&mut edits, declarator.end, format!(" = {zero}"));
                continue;
            }
            // Visible in main only when declared before it.
            if main.is_some_and(|(first, _)| declaration.first < first)
                && let Some(statements) =
                    w.assign(&declarator.name, declaration.ty, &declarator.dims, 0)
            {
                prologue.push(statements);
            }
        }
    }
    if let (false, Some((_, open))) = (prologue.is_empty(), main) {
        w.insert_after(&mut edits, open, format!(" {}", prologue.join(" ")));
    }
    edits.sort_by_key(|e| e.start);
    if edits.windows(2).any(|pair| pair[0].end > pair[1].start) {
        return Err(Unreadable);
    }
    Ok(edits)
}

struct Writer<'r, 't, 'a> {
    t: &'t [PpToken<'a>],
    structures: &'r [Structure],
    es3: bool,
}

impl Writer<'_, '_, '_> {
    /// The constructor of a zero `ty` both languages accept as a constant initializer: none for a structure with no
    /// name or with an array in it.
    fn zero(&self, ty: Type) -> Option<String> {
        Some(match ty {
            Type::Scalar(s) => s.zero().to_owned(),
            Type::Vector(s, n) => {
                let prefix = match s {
                    Scalar::Float => "vec",
                    Scalar::Int => "ivec",
                    Scalar::Uint => "uvec",
                    Scalar::Bool => "bvec",
                };
                format!("{prefix}{n}({})", s.zero())
            }
            Type::Matrix(c, r) if c == r => format!("mat{c}(0.0)"),
            Type::Matrix(c, r) => format!("mat{c}x{r}(0.0)"),
            Type::Struct(index) => {
                let structure = &self.structures[index];
                let name = structure.name.as_ref()?;
                let fields = structure
                    .fields
                    .iter()
                    .map(|f| {
                        if f.dims.is_empty() {
                            self.zero(f.ty)
                        } else {
                            None
                        }
                    })
                    .collect::<Option<Vec<_>>>()?;
                format!("{name}({})", fields.join(", "))
            }
            Type::Opaque => return None,
        })
    }

    /// Statements that set `lhs` to zero.
    fn assign(&self, lhs: &str, ty: Type, dims: &[Dim], depth: usize) -> Option<String> {
        if let Some((outer, inner)) = dims.split_first() {
            let index = format!("_webgl_zi{depth}");
            let size = if self.es3 {
                format!("{lhs}.length()")
            } else {
                outer.0.clone()
            };
            if size.is_empty() {
                return None;
            }
            let body = self.assign(&format!("{lhs}[{index}]"), ty, inner, depth + 1)?;
            return Some(format!(
                "for (int {index} = 0; {index} < {size}; ++{index}) {{ {body} }}"
            ));
        }
        if let Some(zero) = self.zero(ty) {
            return Some(format!("{lhs} = {zero};"));
        }
        let Type::Struct(index) = ty else { return None };
        let fields = self.structures[index]
            .fields
            .iter()
            .map(|f| self.assign(&format!("{lhs}.{}", f.name), f.ty, &f.dims, depth))
            .collect::<Option<Vec<_>>>()?;
        Some(fields.join(" "))
    }

    fn insert_after(&self, edits: &mut Vec<Edit>, token: usize, text: String) {
        if let Some((_, end)) = self.t[token].span {
            edits.push(Edit {
                start: end,
                end,
                text,
            });
        }
    }
}

struct Reader<'t, 'a> {
    t: &'t [PpToken<'a>],
    i: usize,
    structures: Vec<Structure>,
    names: HashMap<String, usize>,
    es3: bool,
    edits: Vec<Edit>,
    globals: Vec<Declaration>,
    /// `main`'s first token and the brace that opens its body.
    main: Option<(usize, usize)>,
}

impl Reader<'_, '_> {
    fn text(&self, at: usize) -> &str {
        self.t.get(at).map_or("", |t| &t.text)
    }

    fn here(&self) -> &str {
        self.text(self.i)
    }

    fn is_ident(&self, at: usize) -> bool {
        self.t.get(at).is_some_and(PpToken::is_ident)
    }

    fn expect(&mut self, text: &str) -> Result<(), Unreadable> {
        if self.here() != text {
            return Err(Unreadable);
        }
        self.i += 1;
        Ok(())
    }

    /// Past the bracket that closes the one at the cursor.
    fn skip_balanced(&mut self) -> Result<(), Unreadable> {
        let mut depth = 0usize;
        loop {
            match self.t.get(self.i).ok_or(Unreadable)?.text.as_ref() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => {
                    depth = depth.checked_sub(1).ok_or(Unreadable)?;
                    if depth == 0 {
                        self.i += 1;
                        return Ok(());
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
    }

    /// To the first of `stops` outside brackets; the cursor is on it.
    fn skip_to(&mut self, stops: &[&str]) -> Result<(), Unreadable> {
        loop {
            let token = self.t.get(self.i).ok_or(Unreadable)?;
            match token.text.as_ref() {
                "(" | "[" | "{" => {
                    self.skip_balanced()?;
                    continue;
                }
                ")" | "]" | "}" => return Err(Unreadable),
                text if stops.contains(&text) => return Ok(()),
                _ => {}
            }
            self.i += 1;
        }
    }

    fn type_name(&self, at: usize) -> Option<Type> {
        let token = self.t.get(at).filter(|t| t.is_ident())?;
        builtin(&token.text).or_else(|| {
            self.names
                .get(token.text.as_ref())
                .map(|&s| Type::Struct(s))
        })
    }

    /// Qualifiers at the cursor: whether none of them stores the variable anywhere but where it is declared.
    fn qualifiers(&mut self) -> Result<bool, Unreadable> {
        let mut ordinary = true;
        loop {
            let word = self.here();
            if word == "layout" {
                ordinary = false;
                self.i += 1;
                self.skip_balanced()?;
            } else if STORAGE.contains(&word) {
                ordinary = false;
                self.i += 1;
            } else if OTHER_QUALIFIERS.contains(&word) {
                self.i += 1;
            } else {
                return Ok(ordinary);
            }
        }
    }

    /// The type at the cursor, and the name of the structure when the type defines one.
    fn type_specifier(&mut self) -> Result<(Type, Option<Option<String>>), Unreadable> {
        if self.here() == "struct" {
            self.i += 1;
            let name = if self.is_ident(self.i) {
                self.i += 1;
                Some(self.text(self.i - 1).to_owned())
            } else {
                None
            };
            self.expect("{")?;
            let mut fields = Vec::new();
            while self.here() != "}" {
                self.qualifiers()?;
                let (ty, _) = self.type_specifier()?;
                let type_dims = self.dims()?;
                loop {
                    if !self.is_ident(self.i) {
                        return Err(Unreadable);
                    }
                    let name = self.here().to_owned();
                    self.i += 1;
                    let mut dims = type_dims.clone();
                    dims.extend(self.dims()?);
                    fields.push(Field { name, ty, dims });
                    match self.here() {
                        "," => self.i += 1,
                        ";" => {
                            self.i += 1;
                            break;
                        }
                        _ => return Err(Unreadable),
                    }
                }
            }
            self.i += 1;
            let index = self.structures.len();
            if let Some(name) = &name {
                self.names.insert(name.clone(), index);
            }
            self.structures.push(Structure {
                name: name.clone(),
                fields,
            });
            return Ok((Type::Struct(index), Some(name)));
        }
        let ty = self.type_name(self.i).ok_or(Unreadable)?;
        self.i += 1;
        Ok((ty, None))
    }

    fn dims(&mut self) -> Result<Vec<Dim>, Unreadable> {
        let mut dims = Vec::new();
        while self.here() == "[" {
            let open = self.i;
            self.skip_balanced()?;
            let size: Vec<&str> = self.t[open + 1..self.i - 1]
                .iter()
                .map(|t| t.text.as_ref())
                .collect();
            dims.push(Dim(size.join(" ")));
        }
        Ok(dims)
    }

    fn translation_unit(&mut self) -> Result<(), Unreadable> {
        while self.i < self.t.len() {
            match self.here() {
                ";" => self.i += 1,
                "precision" => {
                    self.skip_to(&[";"])?;
                    self.i += 1;
                }
                _ => self.external_declaration()?,
            }
        }
        Ok(())
    }

    fn external_declaration(&mut self) -> Result<(), Unreadable> {
        let first = self.i;
        let ordinary = self.qualifiers()?;
        // Qualifiers alone set a default: `layout(std140) uniform;`.
        if self.here() == ";" && first != self.i {
            self.i += 1;
            return Ok(());
        }
        if self.here() != "struct" && self.type_name(self.i).is_none() {
            // An interface block, or a redeclaration such as `invariant gl_Position;`.
            if !self.is_ident(self.i) || first == self.i {
                return Err(Unreadable);
            }
            if self.text(self.i + 1) == "{" {
                self.i += 1;
                self.skip_balanced()?;
            }
            self.skip_to(&[";"])?;
            self.i += 1;
            return Ok(());
        }
        let (ty, defines) = self.type_specifier()?;
        let type_dims = self.dims()?;
        if self.here() == ";" {
            self.i += 1;
            return Ok(());
        }
        if self.is_ident(self.i) && self.text(self.i + 1) == "(" {
            let is_main = self.here() == "main";
            self.i += 1;
            self.skip_balanced()?;
            if self.here() == "{" {
                if is_main {
                    self.main = Some((first, self.i));
                }
                return self.block();
            }
            return self.expect(";");
        }
        let declaration = self.declarators(first, ty, defines.flatten(), type_dims, ordinary)?;
        self.globals.push(declaration);
        Ok(())
    }

    fn declarators(
        &mut self,
        first: usize,
        ty: Type,
        defines: Option<String>,
        type_dims: Vec<Dim>,
        ordinary: bool,
    ) -> Result<Declaration, Unreadable> {
        let before_name = self.i - 1;
        let mut declarators = Vec::new();
        loop {
            if !self.is_ident(self.i) {
                return Err(Unreadable);
            }
            let name = self.here().to_owned();
            self.i += 1;
            let mut dims = type_dims.clone();
            dims.extend(self.dims()?);
            let end = self.i - 1;
            let initialized = self.here() == "=";
            if initialized {
                self.skip_to(&[",", ";"])?;
            }
            let comma = (self.here() == ",").then_some(self.i);
            declarators.push(Declarator {
                name,
                dims,
                end,
                initialized,
                comma,
            });
            match self.here() {
                "," => self.i += 1,
                ";" => {
                    self.i += 1;
                    return Ok(Declaration {
                        first,
                        before_name,
                        ty,
                        defines,
                        semicolon: self.i - 1,
                        declarators,
                        ordinary,
                    });
                }
                _ => return Err(Unreadable),
            }
        }
    }

    fn block(&mut self) -> Result<(), Unreadable> {
        self.expect("{")?;
        while self.here() != "}" {
            if self.i >= self.t.len() {
                return Err(Unreadable);
            }
            self.statement(false)?;
        }
        self.i += 1;
        Ok(())
    }

    /// One statement. `alone`: it is the whole body of an `if`, a loop or an `else`, with no block of its own.
    fn statement(&mut self, alone: bool) -> Result<(), Unreadable> {
        match self.here() {
            "{" => self.block(),
            ";" => {
                self.i += 1;
                Ok(())
            }
            "if" => {
                self.i += 1;
                self.skip_balanced()?;
                self.statement(true)?;
                if self.here() == "else" {
                    self.i += 1;
                    self.statement(true)?;
                }
                Ok(())
            }
            "for" => {
                self.i += 1;
                self.expect("(")?;
                if self.here() == ";" {
                    self.i += 1;
                } else if self.declaration_start() {
                    self.local_declaration(true)?;
                } else {
                    self.skip_to(&[";"])?;
                    self.i += 1;
                }
                // The condition, then the expression after it, which the header's parenthesis closes.
                self.skip_to(&[";"])?;
                self.i += 1;
                while self.here() != ")" {
                    match self.here() {
                        "(" | "[" | "{" => self.skip_balanced()?,
                        "" | "]" | "}" => return Err(Unreadable),
                        _ => self.i += 1,
                    }
                }
                self.i += 1;
                self.statement(true)
            }
            "while" => {
                self.i += 1;
                self.skip_balanced()?;
                self.statement(true)
            }
            "do" => {
                self.i += 1;
                self.statement(true)?;
                self.expect("while")?;
                self.skip_balanced()?;
                self.expect(";")
            }
            "switch" => {
                self.i += 1;
                self.skip_balanced()?;
                self.expect("{")?;
                while self.here() != "}" {
                    match self.here() {
                        "case" => {
                            self.skip_to(&[":"])?;
                            self.i += 1;
                        }
                        "default" => {
                            self.i += 1;
                            self.expect(":")?;
                        }
                        "" => return Err(Unreadable),
                        _ => self.statement(false)?,
                    }
                }
                self.i += 1;
                Ok(())
            }
            "return" | "break" | "continue" | "discard" => {
                self.skip_to(&[";"])?;
                self.i += 1;
                Ok(())
            }
            _ if self.declaration_start() => self.local_declaration(alone),
            _ => {
                self.skip_to(&[";"])?;
                self.i += 1;
                Ok(())
            }
        }
    }

    /// A declaration starts at the cursor, not an expression: a type (perhaps qualified, perhaps an array type) and
    /// then a name -- `vec3(1.0);` is a constructor's value, `S.x = 1.0;` an assignment.
    fn declaration_start(&self) -> bool {
        let mut at = self.i;
        while STORAGE.contains(&self.text(at)) || OTHER_QUALIFIERS.contains(&self.text(at)) {
            at += 1;
        }
        if self.text(at) == "struct" {
            return true;
        }
        if self.type_name(at).is_none() {
            return false;
        }
        at += 1;
        while self.text(at) == "[" {
            let mut depth = 0;
            loop {
                match self.text(at) {
                    "[" => depth += 1,
                    "]" => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    "" => return false,
                    _ => {}
                }
                at += 1;
            }
            at += 1;
        }
        self.is_ident(at)
    }

    /// A declaration in a function. `alone`: the whole body of an `if` or a loop, or a `for`'s first clause -- no
    /// statement can follow it in its scope.
    fn local_declaration(&mut self, alone: bool) -> Result<(), Unreadable> {
        let first = self.i;
        let ordinary = self.qualifiers()?;
        let is_const = (first..self.i).any(|at| self.text(at) == "const");
        let (ty, defines) = self.type_specifier()?;
        let type_dims = self.dims()?;
        if self.here() == ";" {
            self.i += 1;
            return Ok(());
        }
        let anonymous = matches!(defines, Some(None));
        let declaration = self.declarators(first, ty, defines.flatten(), type_dims, ordinary)?;
        if is_const {
            return Ok(());
        }
        let writer = Writer {
            t: self.t,
            structures: &self.structures,
            es3: self.es3,
        };
        let last = declaration.declarators.len() - 1;
        for (n, declarator) in declaration
            .declarators
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.initialized)
        {
            if declarator.dims.is_empty()
                && let Some(zero) = writer.zero(declaration.ty)
            {
                writer.insert_after(&mut self.edits, declarator.end, format!(" = {zero}"));
                continue;
            }
            if alone {
                continue;
            }
            let Some(statements) =
                writer.assign(&declarator.name, declaration.ty, &declarator.dims, 0)
            else {
                continue;
            };
            if n == last {
                writer.insert_after(
                    &mut self.edits,
                    declaration.semicolon,
                    format!(" {statements}"),
                );
                continue;
            }
            // A declarator after this one may read it: the declaration is split here, and the rest declared again.
            let Some(comma) = declarator.comma.and_then(|c| self.t[c].span) else {
                continue;
            };
            let again = match (&declaration.defines, anonymous) {
                (_, true) => continue,
                (Some(name), _) => name.clone(),
                (None, _) => {
                    // The qualifiers and the type, as the source wrote them.
                    let words = &self.t[declaration.first..=declaration.before_name];
                    if words.iter().any(|t| t.span.is_none()) {
                        continue;
                    }
                    words
                        .iter()
                        .map(|t| t.text.as_ref())
                        .collect::<Vec<_>>()
                        .join(" ")
                }
            };
            self.edits.push(Edit {
                start: comma.0,
                end: comma.1,
                text: format!("; {statements} {again}"),
            });
        }
        Ok(())
    }
}
