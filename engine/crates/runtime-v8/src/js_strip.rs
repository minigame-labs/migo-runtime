//! The engine JS as a snapshot keeps it: without comments or indentation, with every line where it was.
//!
//! A snapshot stores each extension module's source text, and every isolate deserialises it onto its
//! heap -- comments included, a third of the 1.4 MB the engine embeds. The snapshot generator hands
//! the sources through [`strip_comments`] first (`deno_core`'s extension transpiler), so the snapshot
//! and each session's heap carry only code. Line terminators stay where they were, so a stack trace
//! still names the right line and automatic semicolon insertion sees what it saw; only columns move.
//!
//! The one hard question is `/`: a regular expression or a division. In general only a parser knows
//! -- `{}` is a block or an object, `x` on the line before may end a `let` -- and a tokenizer that
//! guesses wrong turns `/[/*x*/]/` into `/[ ]/`, a program that still compiles and does something
//! else. Even acorn's tokenizer guesses; its parser re-reads the slash when the guess was wrong. So
//! this one does not guess. It decides `/` only where the token before it settles the grammar --
//! after `(`, `=`, `return` an operand starts; after `2`, `]`, `x.y` one has ended -- and at the first
//! `/` it cannot settle (after `}`, `++`, `of`, or a name on an earlier line) it copies the rest of the
//! file as it stands. Anything else it was not written for -- a non-ASCII byte, an escape in an
//! identifier, an unterminated literal -- returns the input unchanged.

/// What a `/` right after a token is.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Slash {
    /// An operand is due (`(`, `,`, `=`, `return`, the `)` of an `if`): a regular expression.
    Regex,
    /// An operand just ended (`2`, `]`, a template, `x.y`, the `)` of a call): a division.
    Divide,
    /// A name or a string: a division, unless a line terminator came first -- then automatic
    /// semicolon insertion may end the statement (`let x`, `break label`, `import "m"`) and a
    /// regular expression start the next.
    DivideOnSameLine,
    /// Only a parser can tell: `}` ends a block or an object, `++` is prefix or postfix, `of` and
    /// `yield` are keywords or names.
    Undecided,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Open {
    /// `statement`: the `(` of an `if`, `for`, `while` or `with`, after whose `)` a statement starts.
    Paren {
        statement: bool,
    },
    Bracket,
    Brace,
    TemplateSubstitution,
}

/// The previous token, as far as the next word or `(` cares.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Prev {
    /// `.` or `?.`: the next word is a property name, whatever it spells.
    Dot,
    /// `if`, `while`, `with`, or `for await`: the next `(` is the statement's.
    Head,
    /// `for`, which `await` may follow.
    For,
    Other,
}

/// After these an operand starts. A `/` cannot follow `break`, `continue` or `debugger` on their
/// line, so one on the next begins a statement.
const OPERAND_FOLLOWS: &[&str] = &[
    "return",
    "throw",
    "typeof",
    "void",
    "delete",
    "new",
    "in",
    "instanceof",
    "case",
    "do",
    "else",
    "extends",
    "default",
    "break",
    "continue",
    "debugger",
];

/// These are operands.
const OPERANDS: &[&str] = &["this", "super", "null", "true", "false"];

/// A `/` after these is either invalid or a parser's call: `for (x of /re/)` against `of / 2`,
/// `yield` and `await` as operators or names.
const UNDECIDED: &[&str] = &[
    "of", "yield", "await", "let", "var", "const", "function", "class", "if", "for", "while",
    "with", "switch", "try", "catch", "finally", "import", "export", "enum",
];

struct Bail;

struct Stripper<'a> {
    src: &'a [u8],
    i: usize,
    out: Vec<u8>,
    open: Vec<Open>,
    slash: Slash,
    prev: Prev,
    /// A line terminator since the previous token, in a comment or not.
    newline: bool,
    at_line_start: bool,
    /// Where the rest was copied as it stands, at a `/` this could not decide.
    stopped_at: Option<usize>,
}

/// `src` with its comments removed and each line's leading indentation dropped, up to the first `/`
/// this cannot decide; `src` itself when it is anything this was not written for. Line terminators
/// are kept one for one, a CR alone as an LF.
pub fn strip_comments(src: &str) -> String {
    strip(src).map_or_else(|| src.to_string(), |(text, _)| text)
}

/// The stripped text and the offset of the `/` it stopped at, or `None` to leave `src` alone.
fn strip(src: &str) -> Option<(String, Option<usize>)> {
    if !src.is_ascii() {
        return None;
    }
    let mut s = Stripper {
        src: src.as_bytes(),
        i: 0,
        out: Vec::with_capacity(src.len()),
        open: Vec::new(),
        slash: Slash::Regex,
        prev: Prev::Other,
        newline: false,
        at_line_start: true,
        stopped_at: None,
    };
    s.run().ok()?;
    // Only whole ASCII bytes are ever copied or skipped.
    Some((
        String::from_utf8(s.out).expect("ASCII in, ASCII out"),
        s.stopped_at,
    ))
}

fn is_ident_byte(c: u8) -> bool {
    c == b'_' || c == b'$' || c.is_ascii_alphanumeric()
}

impl Stripper<'_> {
    fn peek(&self, offset: usize) -> Option<u8> {
        self.src.get(self.i + offset).copied()
    }

    fn emit(&mut self, byte: u8) {
        self.out.push(byte);
        self.at_line_start = byte == b'\n' || byte == b'\r';
    }

    /// The line terminator at `i` as it goes out. A CR that no LF follows goes out as an LF: once
    /// the comment or indentation after it is gone it could meet one, and CRLF is a single line
    /// terminator where the source had two.
    fn terminator(&self) -> u8 {
        match self.src[self.i] {
            b'\r' if self.peek(1) != Some(b'\n') => b'\n',
            c => c,
        }
    }

    fn copy(&mut self, n: usize) {
        for _ in 0..n {
            let c = self.src[self.i];
            self.emit(c);
            self.i += 1;
        }
    }

    /// Template text is its value: copied as it stands, indentation and line breaks included.
    fn raw(&mut self, n: usize) {
        self.out.extend_from_slice(&self.src[self.i..self.i + n]);
        self.i += n;
        self.at_line_start = false;
    }

    fn token(&mut self, slash: Slash, prev: Prev) {
        self.slash = slash;
        self.prev = prev;
        self.newline = false;
    }

    fn punctuator(&mut self, n: usize, slash: Slash) {
        self.copy(n);
        self.token(slash, Prev::Other);
    }

    fn close(&mut self, expected: Open) -> Result<Open, Bail> {
        match self.open.pop() {
            Some(open) if std::mem::discriminant(&open) == std::mem::discriminant(&expected) => {
                Ok(open)
            }
            _ => Err(Bail),
        }
    }

    fn run(&mut self) -> Result<(), Bail> {
        if self.src.starts_with(b"#!") {
            return Err(Bail);
        }
        while let Some(c) = self.peek(0) {
            match c {
                b'\n' | b'\r' => {
                    let c = self.terminator();
                    self.emit(c);
                    self.i += 1;
                    self.newline = true;
                }
                b' ' | b'\t' | 0x0b | 0x0c if self.at_line_start => self.i += 1,
                b' ' | b'\t' | 0x0b | 0x0c => self.copy(1),
                b'/' if self.peek(1) == Some(b'/') => {
                    while !matches!(self.peek(0), None | Some(b'\n') | Some(b'\r')) {
                        self.i += 1;
                    }
                }
                b'/' if self.peek(1) == Some(b'*') => self.block_comment()?,
                b'/' => {
                    let regex = match self.slash {
                        Slash::Regex => true,
                        Slash::Divide => false,
                        Slash::DivideOnSameLine if !self.newline => false,
                        Slash::DivideOnSameLine | Slash::Undecided => {
                            self.stopped_at = Some(self.i);
                            self.out.extend_from_slice(&self.src[self.i..]);
                            return Ok(());
                        }
                    };
                    if regex {
                        self.regex()?;
                    } else {
                        self.punctuator(
                            if self.peek(1) == Some(b'=') { 2 } else { 1 },
                            Slash::Regex,
                        );
                    }
                }
                b'\'' | b'"' => self.string(c)?,
                b'`' => {
                    self.raw(1);
                    self.template()?;
                }
                b'(' => {
                    let statement = matches!(self.prev, Prev::Head | Prev::For);
                    self.open.push(Open::Paren { statement });
                    self.punctuator(1, Slash::Regex);
                }
                b')' => {
                    let Open::Paren { statement } = self.close(Open::Paren { statement: false })?
                    else {
                        unreachable!("close returns what it was asked for")
                    };
                    self.punctuator(
                        1,
                        if statement {
                            Slash::Regex
                        } else {
                            Slash::Divide
                        },
                    );
                }
                b'[' => {
                    self.open.push(Open::Bracket);
                    self.punctuator(1, Slash::Regex);
                }
                b']' => {
                    self.close(Open::Bracket)?;
                    self.punctuator(1, Slash::Divide);
                }
                b'{' => {
                    self.open.push(Open::Brace);
                    self.punctuator(1, Slash::Regex);
                }
                b'}' if self.open.last() == Some(&Open::TemplateSubstitution) => {
                    self.open.pop();
                    self.raw(1);
                    self.template()?;
                }
                b'}' => {
                    self.close(Open::Brace)?;
                    self.punctuator(1, Slash::Undecided);
                }
                b'.' if self.peek(1).is_some_and(|d| d.is_ascii_digit()) => self.number()?,
                b'.' if self.peek(1) == Some(b'.') && self.peek(2) == Some(b'.') => {
                    self.punctuator(3, Slash::Regex);
                }
                b'.' => {
                    self.copy(1);
                    self.token(Slash::Undecided, Prev::Dot);
                }
                b'?' if self.peek(1) == Some(b'.')
                    && !self.peek(2).is_some_and(|d| d.is_ascii_digit()) =>
                {
                    self.copy(2);
                    self.token(Slash::Undecided, Prev::Dot);
                }
                // HTML-like comments, which a classic script reads where a module reads operators.
                b'<' if self.src[self.i..].starts_with(b"<!--") => return Err(Bail),
                b'-' if self.src[self.i..].starts_with(b"-->") => return Err(Bail),
                b'+' | b'-' if self.peek(1) == Some(c) => self.punctuator(2, Slash::Undecided),
                b'=' | b'+' | b'-' | b'*' | b'%' | b'<' | b'>' | b'&' | b'|' | b'^' | b'!'
                | b'~' | b'?' | b':' | b';' | b',' => self.punctuator(1, Slash::Regex),
                b'0'..=b'9' => self.number()?,
                // A private name is read as a property name: `this.#x / 2`, `#if()`.
                b'#' if self
                    .peek(1)
                    .is_some_and(|d| d == b'_' || d == b'$' || d.is_ascii_alphabetic()) =>
                {
                    self.copy(1);
                    self.prev = Prev::Dot;
                    self.word();
                }
                c if c == b'_' || c == b'$' || c.is_ascii_alphabetic() => self.word(),
                // An escape in an identifier (`ab`), `@`, or a byte no token starts with.
                _ => return Err(Bail),
            }
        }
        if self.open.is_empty() {
            Ok(())
        } else {
            Err(Bail)
        }
    }

    fn word(&mut self) {
        let start = self.i;
        while self.peek(0).is_some_and(is_ident_byte) {
            self.i += 1;
        }
        let word = &self.src[start..self.i];
        self.out.extend_from_slice(word);
        self.at_line_start = false;
        if self.prev == Prev::Dot {
            // `x.return / 2` divides.
            self.token(Slash::Divide, Prev::Other);
            return;
        }
        let word = std::str::from_utf8(word).expect("ASCII");
        let prev = match word {
            "if" | "while" | "with" => Prev::Head,
            "for" => Prev::For,
            "await" if self.prev == Prev::For => Prev::Head,
            _ => Prev::Other,
        };
        let slash = if OPERAND_FOLLOWS.contains(&word) {
            Slash::Regex
        } else if OPERANDS.contains(&word) {
            Slash::Divide
        } else if UNDECIDED.contains(&word) {
            Slash::Undecided
        } else {
            Slash::DivideOnSameLine
        };
        self.token(slash, prev);
    }

    /// A numeric literal, read by its grammar: `0..e+/re/` is `0.`, `.e`, `+` and a regular expression.
    fn number(&mut self) -> Result<(), Bail> {
        let digits = |s: &mut Self| {
            while s.peek(0).is_some_and(|c| c.is_ascii_digit() || c == b'_') {
                s.copy(1);
            }
        };
        if self.peek(0) == Some(b'0') && self.peek(1).is_some_and(|c| b"xXoObB".contains(&c)) {
            self.copy(2);
            while self
                .peek(0)
                .is_some_and(|c| c.is_ascii_hexdigit() || c == b'_')
            {
                self.copy(1);
            }
        } else {
            digits(self);
            if self.peek(0) == Some(b'.') {
                self.copy(1);
                digits(self);
            }
            if matches!(self.peek(0), Some(b'e' | b'E')) {
                self.copy(1);
                if matches!(self.peek(0), Some(b'+' | b'-')) {
                    self.copy(1);
                }
                digits(self);
            }
        }
        if self.peek(0) == Some(b'n') {
            self.copy(1);
        }
        // A name cannot touch a number: `3in x` is not a program.
        if self.peek(0).is_some_and(|c| is_ident_byte(c) || c == b'\\') {
            return Err(Bail);
        }
        self.token(Slash::Divide, Prev::Other);
        Ok(())
    }

    fn block_comment(&mut self) -> Result<(), Bail> {
        self.i += 2;
        let mut terminators = Vec::new();
        loop {
            match self.peek(0) {
                None => return Err(Bail),
                Some(b'*') if self.peek(1) == Some(b'/') => {
                    self.i += 2;
                    break;
                }
                Some(b'\n' | b'\r') => {
                    terminators.push(self.terminator());
                    self.i += 1;
                }
                Some(_) => self.i += 1,
            }
        }
        if terminators.is_empty() {
            // `a/**/b` must not become `ab`.
            if !self.at_line_start {
                self.emit(b' ');
            }
        } else {
            for c in terminators {
                self.emit(c);
            }
            self.newline = true;
        }
        Ok(())
    }

    fn string(&mut self, quote: u8) -> Result<(), Bail> {
        self.copy(1);
        loop {
            match self.peek(0) {
                None | Some(b'\n') | Some(b'\r') => return Err(Bail),
                Some(b'\\') => {
                    // A line continuation counts its CRLF as one terminator.
                    let n = if self.peek(1) == Some(b'\r') && self.peek(2) == Some(b'\n') {
                        3
                    } else {
                        2
                    };
                    if self.i + n > self.src.len() {
                        return Err(Bail);
                    }
                    self.copy(n);
                }
                Some(c) if c == quote => {
                    self.copy(1);
                    break;
                }
                Some(_) => self.copy(1),
            }
        }
        self.token(Slash::DivideOnSameLine, Prev::Other);
        Ok(())
    }

    /// Template text after a backtick or a substitution's `}`, up to the closing backtick or the next `${`.
    fn template(&mut self) -> Result<(), Bail> {
        loop {
            match self.peek(0) {
                None => return Err(Bail),
                Some(b'\\') => {
                    if self.i + 2 > self.src.len() {
                        return Err(Bail);
                    }
                    self.raw(2);
                }
                Some(b'`') => {
                    self.raw(1);
                    self.token(Slash::Divide, Prev::Other);
                    return Ok(());
                }
                Some(b'$') if self.peek(1) == Some(b'{') => {
                    self.raw(2);
                    self.open.push(Open::TemplateSubstitution);
                    self.token(Slash::Regex, Prev::Other);
                    return Ok(());
                }
                Some(_) => self.raw(1),
            }
        }
    }

    /// A regular expression literal by its lexical grammar, which no flag changes: a `/` inside a
    /// class does not end it, and a class does not nest.
    fn regex(&mut self) -> Result<(), Bail> {
        self.copy(1);
        let mut in_class = false;
        loop {
            match self.peek(0) {
                None | Some(b'\n') | Some(b'\r') => return Err(Bail),
                Some(b'\\') => {
                    if matches!(self.peek(1), None | Some(b'\n') | Some(b'\r')) {
                        return Err(Bail);
                    }
                    self.copy(2);
                }
                Some(b'[') => {
                    in_class = true;
                    self.copy(1);
                }
                Some(b']') => {
                    in_class = false;
                    self.copy(1);
                }
                Some(b'/') if !in_class => {
                    self.copy(1);
                    break;
                }
                Some(_) => self.copy(1),
            }
        }
        while self.peek(0).is_some_and(is_ident_byte) {
            self.copy(1);
        }
        self.token(Slash::Divide, Prev::Other);
        Ok(())
    }
}

/// What `deno_core::snapshot::create_snapshot` takes as its extension transpiler: every extension
/// module, stripped, before it is evaluated into the snapshot's heap.
pub type SnapshotTranspiler = dyn Fn(
    deno_core::ModuleName,
    deno_core::ModuleCodeString,
) -> Result<
    (
        deno_core::ModuleCodeString,
        Option<deno_core::SourceMapData>,
    ),
    deno_error::JsErrorBox,
>;

/// The transpiler the snapshot generator installs.
pub fn snapshot_transpiler() -> std::rc::Rc<SnapshotTranspiler> {
    std::rc::Rc::new(|_name, code| Ok((strip_comments(code.as_str()).into(), None)))
}

#[cfg(test)]
mod tests {
    use super::{strip, strip_comments};

    #[test]
    fn comments_go_and_everything_quoted_stays() {
        let cases = [
            ("a; // note\nb;", "a; \nb;"),
            ("a /* x */ + b", "a   + b"),
            ("a/**/b", "a b"),
            ("x = '// not a comment';", "x = '// not a comment';"),
            ("x = \"/* nor this */\";", "x = \"/* nor this */\";"),
            ("x = 'it\\'s // here';", "x = 'it\\'s // here';"),
            (
                "x = `a // b ${ y /* c */ } d /* e */`;",
                "x = `a // b ${ y   } d /* e */`;",
            ),
            ("x = `${ `${ 1 } // in` }`;", "x = `${ `${ 1 } // in` }`;"),
            ("x = `${ {a: 1}.a /* c */ }`;", "x = `${ {a: 1}.a   }`;"),
            (
                "return /\\/\\/ x/.test(s); // c",
                "return /\\/\\/ x/.test(s); ",
            ),
            ("r = /[/]/// c\n", "r = /[/]/\n"),
            ("r = /[/]// c\n", "r = /[/]// c\n"),
            ("q = a / b / c; // div", "q = a / b / c; "),
            ("q = (a) / 2 /* d */;", "q = (a) / 2  ;"),
            ("q = a[0] / 2 /* d */;", "q = a[0] / 2  ;"),
            ("q = 1.5e-3 / 2 // d", "q = 1.5e-3 / 2 "),
            ("if (x) {\n    y();\n}", "if (x) {\ny();\n}"),
            ("s = `\n    kept\n`;", "s = `\n    kept\n`;"),
            ("/*\n * two\n */\nz;", "\n\n\nz;"),
            (
                "o = { m() { return 1 } }; // c",
                "o = { m() { return 1 } }; ",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(strip_comments(input), expected, "input: {input:?}");
        }
    }

    /// Where the token before a `/` settles it, the `/` is read and stripping goes on.
    #[test]
    fn a_slash_the_previous_token_settles_is_read() {
        for (input, expected) in [
            (
                "if (true) /[/*x*/]/.test(s); // c",
                "if (true) /[/*x*/]/.test(s); ",
            ),
            (
                "for await (const x of y) /[/*x*/]/.test(x); // c",
                "for await (const x of y) /[/*x*/]/.test(x); ",
            ),
            (
                "x = (a) / /[/*x*/]/.lastIndex; // c",
                "x = (a) / /[/*x*/]/.lastIndex; ",
            ),
            (
                "x = y.return / /[/*x*/]/.lastIndex; // c",
                "x = y.return / /[/*x*/]/.lastIndex; ",
            ),
            (
                "x = `${a}` / /[/*x*/]/.lastIndex; // c",
                "x = `${a}` / /[/*x*/]/.lastIndex; ",
            ),
            (
                "x = this / /[/*x*/]/.lastIndex; // c",
                "x = this / /[/*x*/]/.lastIndex; ",
            ),
            (
                "x = 0..e+/[/*x*/]/.source; // c",
                "x = 0..e+/[/*x*/]/.source; ",
            ),
            (
                "debugger\n/[/*x*/]/.test(s); // c",
                "debugger\n/[/*x*/]/.test(s); ",
            ),
            ("x = a.b\n/ 2; // c", "x = a.b\n/ 2; "),
        ] {
            assert_eq!(
                strip(input),
                Some((expected.to_string(), None)),
                "input: {input:?}"
            );
        }
    }

    /// Where only a parser could tell, the rest of the file is copied as it stands: whichever way
    /// the `/` goes, `[/*x*/]` is not touched.
    #[test]
    fn a_slash_only_a_parser_could_read_stops_stripping() {
        for input in [
            "{}\n/[/*x*/]/.test(s); // c",
            "x = {valueOf: () => 6} / /[/*x*/]/.test(\"*\");",
            "x = function () {} / /[/*x*/]/.flags.length;",
            "function f() {}\n/[/*x*/]/.test(s);",
            "class A { m() {} }\n/[/*x*/]/.test(s);",
            "try {} catch {} /[/*x*/]/.test(\"*\");",
            "export default async function f() {}\n/[/*x*/]/.test(\"*\");",
            "export { a }\n/[/*x*/]/.test(s);",
            "class C { static { {} /[/*x*/]/.test(\"*\"); } }",
            "x\n{} /[/*x*/]/.test(\"*\");",
            "false ? 0 :\n{ valueOf() { return 6; } } / /[/*x*/]/.test(\"*\");",
            "for (const of of /[/*x*/]/.source);",
            "for (const m of /[/*x*/]/g.exec(s));",
            "a = b\n/[/*x*/]/g.exec(c);",
            "let x\n/[/*x*/]/.test(\"*\");",
            "import \"m\"\n/[/*x*/]/.test(\"*\");",
            "a: { break a\n/[/*x*/]/.test(\"*\"); }",
            "x = y++ / /[/*x*/]/.lastIndex;",
            "async function* g() { yield /[/*x*/]/; }",
        ] {
            let (text, stopped) = strip(input).unwrap_or_else(|| panic!("left alone: {input:?}"));
            assert_eq!(text, input, "input: {input:?}");
            assert!(stopped.is_some(), "input: {input:?}");
        }
        // What came before the stop is stripped.
        assert_eq!(strip_comments("a; // c\n{} / 2 // d"), "a; \n{} / 2 // d");
    }

    /// The decisive check: V8 runs each program before and after, and gets the same answer.
    #[test]
    fn v8_gets_the_same_answer_before_and_after() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        fn run(src: &str) -> String {
            let mut runtime = deno_core::JsRuntime::new(Default::default());
            let value = runtime
                .execute_script("<case>", deno_core::FastString::from(src.to_string()))
                .unwrap_or_else(|e| panic!("{src:?} throws: {e}"));
            deno_core::scope!(scope, &mut runtime);
            deno_core::v8::Local::new(scope, value).to_rust_string_lossy(scope)
        }
        for src in [
            "if (true) /* c */ /[/*x*/]/.test(\"*\") // c",
            "let a = 2; (a) / /[/*x*/]/.source.length // c",
            "const o = { return: 9 }; o.return / /[/*x*/]/.source.length // c",
            "`${1}` / /[/*x*/]/.source.length // c",
            "0..e+/[/*x*/]/.source // c",
            "debugger\n/[/*x*/]/.test(\"*\") // c",
            "let r = \"\"; for (const c of \"ab\") { if (c === \"a\") continue\n/[/*x*/]/.test(\"*\") && (r += c) } r",
            "let r = false; a: { if (r) break a\n/[/*x*/]/.test(\"*\") && (r = true) } r",
            "var v\n/[/*x*/]/.test(\"*\")",
            "const async = 0; const y = async\nfunction f() {}\n/[/*x*/]/.test(\"*\")",
            "class C { static { {} /[/*x*/]/.test(\"*\") && (C.hit = true); } }; C.hit",
            "const z = 0; z\n{} /[/*x*/]/.test(\"*\")",
            "false ? 0 :\n { valueOf() { return 6; } } / /[/*x*/]/.test(\"*\")",
            "let w = class extends [Object][true ? 0 : 0] { static valueOf() { return 6; } } / /[/*x*/]/.test(\"*\"); w",
            "let r = \"\"; for (const of of /[/*x*/]/.source) r += of; r",
            "let n = 1; n++ / /[/*x*/]/.source.length // c",
            "let q = 8; q /* c */ / 2 // d",
            // Line numbers: CRLF is one terminator, so a CR and an LF must not end up side by side.
            "/*\r*/\n\r  \n\r// c\nnew Error().stack.match(/case>:(\\d+)/)[1]",
        ] {
            let stripped = strip_comments(src);
            assert_eq!(run(&stripped), run(src), "{src:?} became {stripped:?}");
        }
    }

    /// CR is a line terminator too: dropping it would move automatic semicolon insertion.
    #[test]
    fn every_line_terminator_survives() {
        // The CR keeps `return` and `1` on separate lines; the space after it was indentation.
        assert_eq!(strip_comments("return /*\r*/ 1"), "return \n1");
        assert_eq!(strip_comments("// comment\rn = 2;"), "\nn = 2;");
        assert_eq!(strip_comments("a;\r\n  // c\r\nb;"), "a;\r\n\r\nb;");
        // CRLF is one terminator: what is removed between a CR and an LF must not pair them.
        assert_eq!(strip_comments("/*\r*/\nx"), "\n\nx");
        assert_eq!(strip_comments("a\r  \nb"), "a\n\nb");
        assert_eq!(strip_comments("a\r// c\nb"), "a\n\nb");
        assert_eq!(strip_comments("a\r/* c */\nb"), "a\n\nb");
        assert_eq!(
            strip_comments("x = 'a\\\r\n// b'; // c"),
            "x = 'a\\\r\n// b'; "
        );
    }

    /// What it was not written for comes back as it went in.
    #[test]
    fn the_unexpected_is_left_alone() {
        for input in [
            "x = 1; // \u{e9}",
            "x = 1; /* \u{2028} */ y = 2;",
            "a\\u0062 = 1; // c",
            "x = 'unterminated; // c",
            "x = `open ${ y; // c",
            "x = (1; // c",
            "x = [1); // c",
            "y = 1 }; // c",
            "r = /open\n; // c",
            "#!/usr/bin/env node\nx; // c",
            "x = 3in y; // c",
            "x = a <!-- b; // c",
            "--> b; // c",
            "@dec class A {} // c",
        ] {
            assert_eq!(strip(input), None, "input: {input:?}");
        }
    }

    fn embedded_js() -> Vec<(std::path::PathBuf, String)> {
        let mut files = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read the crate's source tree") {
                let path = entry.expect("a directory entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "js") {
                    let text = std::fs::read_to_string(&path).expect("engine JS is UTF-8");
                    files.push((path, text));
                }
            }
        }
        assert!(files.len() > 50, "found only {} JS files", files.len());
        files
    }

    /// Every file the engine embeds is stripped to its end (none fell back to its input or stopped
    /// at a `/` it could not decide), keeps its line terminators, and is a fixed point.
    #[test]
    fn every_embedded_file_strips_keeps_its_lines_and_is_a_fixed_point() {
        let terminators = |s: &str| s.bytes().filter(|b| *b == b'\n' || *b == b'\r').count();
        let (mut before, mut after) = (0, 0);
        for (path, text) in embedded_js() {
            let (once, stopped) =
                strip(&text).unwrap_or_else(|| panic!("{}: left unstripped", path.display()));
            if let Some(at) = stopped {
                let line = text[..at].lines().count();
                panic!(
                    "{}:{line}: stopped at a `/` only a parser could read",
                    path.display()
                );
            }
            assert_eq!(
                terminators(&once),
                terminators(&text),
                "{}: line terminators moved",
                path.display()
            );
            assert_eq!(
                strip_comments(&once),
                once,
                "{}: stripping twice changed it",
                path.display()
            );
            before += text.len();
            after += once.len();
        }
        assert!(
            after * 10 < before * 8,
            "stripped {before} bytes only to {after}"
        );
    }

    /// V8 parses every stripped file as the module it is.
    #[test]
    fn every_embedded_file_still_compiles() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        use deno_core::v8;
        let mut runtime = deno_core::JsRuntime::new(Default::default());
        deno_core::scope!(scope, &mut runtime);
        for (path, text) in embedded_js() {
            let stripped = strip_comments(&text);
            v8::tc_scope!(let tc, scope);
            let name = v8::String::new(tc, &path.display().to_string()).unwrap();
            let origin = v8::ScriptOrigin::new(
                tc,
                name.into(),
                0,
                0,
                false,
                0,
                None,
                false,
                false,
                true,
                None,
            );
            let code = v8::String::new(tc, &stripped).unwrap();
            let mut source = v8::script_compiler::Source::new(code, Some(&origin));
            let compiled = v8::script_compiler::compile_module(tc, &mut source).is_some();
            let error = tc.exception().map(|e| e.to_rust_string_lossy(tc));
            assert!(
                compiled,
                "{} no longer compiles once stripped: {error:?}",
                path.display()
            );
        }
    }
}
