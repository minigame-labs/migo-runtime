//! The GLSL ES preprocessor, as far as reading a shader needs it: the code a driver compiles, with every macro
//! expanded and every inactive conditional block gone.
//!
//! It does not produce the shader the driver gets -- the driver preprocesses the source itself -- so its macros are
//! the driver's: `GL_ES`, `__VERSION__`, `GL_FRAGMENT_PRECISION_HIGH` and one per extension the driver names. A token
//! that comes from the source as it stands keeps its place there; one a macro made has none, because text inserted
//! next to it would land in a macro's body or arguments.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use super::lexer::{lines, Kind, Token};

/// What the driver predefines for a shader.
pub(crate) struct Environment<'e> {
    pub fragment: bool,
    /// The driver's extensions: each `GL_` name is a macro it defines.
    pub extensions: &'e HashSet<String>,
}

#[derive(Clone, Debug)]
pub(super) struct PpToken<'a> {
    pub kind: Kind,
    pub text: Cow<'a, str>,
    /// Where the token is in the source: `None` when a macro expansion made it.
    pub span: Option<(usize, usize)>,
    /// Macros this token may no longer expand: the ones whose expansion it came from.
    hide: Rc<Vec<&'a str>>,
}

impl PpToken<'_> {
    pub fn is(&self, text: &str) -> bool {
        self.text == text
    }

    pub fn is_ident(&self) -> bool {
        self.kind == Kind::Ident
    }
}

/// The code a driver compiles, and the shading language version it is in.
pub(super) struct Preprocessed<'a> {
    pub tokens: Vec<PpToken<'a>>,
    /// 100 or 300.
    pub version: u32,
}

/// The shader could not be read as the driver would; the driver reports what is wrong.
#[derive(Debug)]
pub(super) struct Unreadable;

struct Macro<'a> {
    params: Option<Vec<&'a str>>,
    body: Vec<Token<'a>>,
}

struct Conditional {
    /// This block's code is compiled.
    active: bool,
    /// A block of this `#if` chain has been compiled.
    taken: bool,
    /// The enclosing block's code is compiled.
    outer: bool,
    seen_else: bool,
}

pub(super) fn preprocess<'a>(source: &'a str, env: &Environment) -> Result<Preprocessed<'a>, Unreadable> {
    let mut pp = Preprocessor {
        macros: HashMap::new(),
        version: 100,
        env,
    };
    let mut conditionals: Vec<Conditional> = Vec::new();
    let mut tokens = Vec::new();
    let mut run: Vec<PpToken<'a>> = Vec::new();
    let all = lines(source);
    for (index, line) in all.iter().enumerate() {
        let active = conditionals.last().is_none_or(|c| c.active);
        if !line.is_directive() {
            if active {
                run.extend(line.tokens.iter().map(|t| pp.own(t)));
            }
            continue;
        }
        // A directive ends the run of text before it: a macro call does not span one.
        if !run.is_empty() {
            tokens.extend(pp.expand(std::mem::take(&mut run))?);
        }
        let words = &line.tokens[1..];
        let Some(name) = words.first() else { continue };
        match name.text {
            "if" | "ifdef" | "ifndef" => {
                let value = if active { pp.condition(name.text, &words[1..])? } else { false };
                conditionals.push(Conditional {
                    active: active && value,
                    taken: value,
                    outer: active,
                    seen_else: false,
                });
            }
            "elif" => {
                let c = conditionals.last_mut().ok_or(Unreadable)?;
                if c.seen_else {
                    return Err(Unreadable);
                }
                let value = c.outer && !c.taken && pp.condition("if", &words[1..])?;
                c.active = value;
                c.taken |= value;
            }
            "else" => {
                let c = conditionals.last_mut().ok_or(Unreadable)?;
                if c.seen_else {
                    return Err(Unreadable);
                }
                c.seen_else = true;
                c.active = c.outer && !c.taken;
                c.taken = true;
            }
            "endif" => {
                conditionals.pop().ok_or(Unreadable)?;
            }
            _ if !active => {}
            "define" => pp.define(&words[1..])?,
            "undef" => {
                let macro_name = words.get(1).ok_or(Unreadable)?;
                pp.macros.remove(macro_name.text);
            }
            "version" => {
                // Only the first line may say it; the driver refuses it anywhere else.
                if index != 0 {
                    return Err(Unreadable);
                }
                pp.version = words.get(1).and_then(|n| n.text.parse().ok()).ok_or(Unreadable)?;
            }
            // Line numbers mean nothing to what is read here.
            "line" | "extension" | "pragma" => {}
            // An active #error, or a directive the language does not have: the driver refuses the shader.
            _ => return Err(Unreadable),
        }
    }
    if !conditionals.is_empty() {
        return Err(Unreadable);
    }
    if !run.is_empty() {
        tokens.extend(pp.expand(run)?);
    }
    // WebGL's two shading languages; anything else the driver refuses.
    if pp.version != 100 && pp.version != 300 {
        return Err(Unreadable);
    }
    Ok(Preprocessed {
        tokens,
        version: pp.version,
    })
}

struct Preprocessor<'a, 'e> {
    macros: HashMap<&'a str, Macro<'a>>,
    version: u32,
    env: &'e Environment<'e>,
}

impl<'a> Preprocessor<'a, '_> {
    fn own(&self, token: &Token<'a>) -> PpToken<'a> {
        PpToken {
            kind: token.kind,
            text: Cow::Borrowed(token.text),
            span: Some((token.start, token.end())),
            hide: Rc::new(Vec::new()),
        }
    }

    fn define(&mut self, words: &[Token<'a>]) -> Result<(), Unreadable> {
        let name = words.first().filter(|n| n.kind == Kind::Ident).ok_or(Unreadable)?;
        // A parenthesis right after the name, with no space, makes a function-like macro.
        if words.get(1).is_some_and(|p| p.is("(") && !p.spaced) {
            let mut params = Vec::new();
            let mut i = 2;
            loop {
                let t = words.get(i).ok_or(Unreadable)?;
                if t.is(")") {
                    i += 1;
                    break;
                }
                if t.kind == Kind::Ident {
                    params.push(t.text);
                } else if !t.is(",") {
                    return Err(Unreadable);
                }
                i += 1;
            }
            self.macros.insert(
                name.text,
                Macro {
                    params: Some(params),
                    body: words[i..].to_vec(),
                },
            );
        } else {
            self.macros.insert(
                name.text,
                Macro {
                    params: None,
                    body: words[1..].to_vec(),
                },
            );
        }
        Ok(())
    }

    /// Whether an `#if`, `#ifdef` or `#ifndef` chooses its block.
    fn condition(&self, directive: &str, words: &[Token<'a>]) -> Result<bool, Unreadable> {
        match directive {
            "ifdef" | "ifndef" => {
                let name = words.first().filter(|n| n.kind == Kind::Ident).ok_or(Unreadable)?;
                Ok(self.is_defined(name.text) == (directive == "ifdef"))
            }
            _ => {
                // `defined X` and `defined(X)` are answered before anything is expanded.
                let mut replaced = Vec::new();
                let mut i = 0;
                while i < words.len() {
                    let w = &words[i];
                    if w.kind == Kind::Ident && w.text == "defined" {
                        let (name, next) = if words.get(i + 1).is_some_and(|p| p.is("(")) {
                            let name = words.get(i + 2).ok_or(Unreadable)?;
                            if !words.get(i + 3).is_some_and(|p| p.is(")")) {
                                return Err(Unreadable);
                            }
                            (name, i + 4)
                        } else {
                            (words.get(i + 1).ok_or(Unreadable)?, i + 2)
                        };
                        let value = if self.is_defined(name.text) { "1" } else { "0" };
                        replaced.push(PpToken {
                            kind: Kind::Number,
                            text: Cow::Borrowed(value),
                            span: None,
                            hide: Rc::new(Vec::new()),
                        });
                        i = next;
                    } else {
                        replaced.push(self.own(w));
                        i += 1;
                    }
                }
                let expanded = self.expand(replaced)?;
                let mut parser = Expression {
                    tokens: &expanded,
                    at: 0,
                };
                let value = parser.or()?;
                if parser.at != expanded.len() {
                    return Err(Unreadable);
                }
                Ok(value != 0)
            }
        }
    }

    fn is_defined(&self, name: &str) -> bool {
        self.macros.contains_key(name) || self.predefined(name).is_some()
    }

    /// A macro the driver defines. `__LINE__` reads as 0: no shader decides what it declares by its line number.
    fn predefined(&self, name: &str) -> Option<Cow<'static, str>> {
        match name {
            "GL_ES" => Some(Cow::Borrowed("1")),
            "__FILE__" | "__LINE__" => Some(Cow::Borrowed("0")),
            "__VERSION__" => Some(Cow::Owned(self.version.to_string())),
            // ESSL 3.00 defines it in both stages; ESSL 1.00 where a fragment shader has high precision.
            "GL_FRAGMENT_PRECISION_HIGH" if self.env.fragment || self.version >= 300 => Some(Cow::Borrowed("1")),
            _ if name.starts_with("GL_") && self.env.extensions.contains(name) => {
                Some(Cow::Borrowed("1"))
            }
            _ => None,
        }
    }

    /// `tokens` with every macro expanded.
    fn expand(&self, tokens: Vec<PpToken<'a>>) -> Result<Vec<PpToken<'a>>, Unreadable> {
        let mut queue: VecDeque<PpToken<'a>> = tokens.into();
        let mut out = Vec::new();
        while let Some(token) = queue.pop_front() {
            if token.kind != Kind::Ident {
                out.push(token);
                continue;
            }
            let name: &str = &token.text;
            let Some((macro_name, definition)) = self.macros.get_key_value(name) else {
                out.push(token);
                continue;
            };
            if token.hide.contains(macro_name) {
                out.push(token);
                continue;
            }
            let mut hide = (*token.hide).clone();
            hide.push(macro_name);
            let hide = Rc::new(hide);
            let made = |t: &Token<'a>| PpToken {
                kind: t.kind,
                text: Cow::Borrowed(t.text),
                span: None,
                hide: hide.clone(),
            };
            let replacement: Vec<PpToken<'a>> = match &definition.params {
                None => definition.body.iter().map(made).collect(),
                Some(params) => {
                    // A function-like macro's name without a parenthesis after it is just a name.
                    if !queue.front().is_some_and(|p| p.is("(")) {
                        out.push(token);
                        continue;
                    }
                    queue.pop_front();
                    let mut args: Vec<Vec<PpToken<'a>>> = vec![Vec::new()];
                    let mut depth = 1;
                    loop {
                        let t = queue.pop_front().ok_or(Unreadable)?;
                        if t.is("(") {
                            depth += 1;
                        } else if t.is(")") {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        } else if t.is(",") && depth == 1 {
                            args.push(Vec::new());
                            continue;
                        }
                        args.last_mut().expect("one argument at least").push(t);
                    }
                    if params.is_empty() && args.len() == 1 && args[0].is_empty() {
                        args.clear();
                    }
                    if args.len() != params.len() {
                        return Err(Unreadable);
                    }
                    let expanded: Vec<Vec<PpToken<'a>>> =
                        args.into_iter().map(|a| self.expand(a)).collect::<Result<_, _>>()?;
                    let mut replacement = Vec::new();
                    for t in &definition.body {
                        match params.iter().position(|p| t.kind == Kind::Ident && *p == t.text) {
                            Some(n) => replacement.extend(expanded[n].iter().map(|a| PpToken {
                                span: None,
                                hide: hide.clone(),
                                ..a.clone()
                            })),
                            None => replacement.push(made(t)),
                        }
                    }
                    replacement
                }
            };
            if replacement.iter().any(|t| t.is("##")) {
                // Token pasting: what a shader builds a name with. Rare enough that a shader using it is left to the
                // driver as it is.
                return Err(Unreadable);
            }
            for t in replacement.into_iter().rev() {
                queue.push_front(t);
            }
        }
        // What is left of the driver's own macros.
        for t in &mut out {
            if t.kind == Kind::Ident {
                if let Some(value) = self.predefined(&t.text) {
                    t.kind = Kind::Number;
                    t.text = Cow::Owned(value.into_owned());
                    t.span = None;
                }
            }
        }
        Ok(out)
    }
}

/// An `#if` expression: integers, C's operators without the conditional one.
struct Expression<'t, 'a> {
    tokens: &'t [PpToken<'a>],
    at: usize,
}

impl Expression<'_, '_> {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.at).map(|t| &*t.text)
    }

    fn binary(&mut self, level: usize) -> Result<i64, Unreadable> {
        const LEVELS: [&[&str]; 10] = [
            &["||"],
            &["&&"],
            &["|"],
            &["^"],
            &["&"],
            &["==", "!="],
            &["<", ">", "<=", ">="],
            &["<<", ">>"],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        while let Some(op) = self.peek().filter(|op| LEVELS[level].contains(op)).map(str::to_owned) {
            self.at += 1;
            let right = self.binary(level + 1)?;
            left = match op.as_str() {
                "||" => i64::from(left != 0 || right != 0),
                "&&" => i64::from(left != 0 && right != 0),
                "|" => left | right,
                "^" => left ^ right,
                "&" => left & right,
                "==" => i64::from(left == right),
                "!=" => i64::from(left != right),
                "<" => i64::from(left < right),
                ">" => i64::from(left > right),
                "<=" => i64::from(left <= right),
                ">=" => i64::from(left >= right),
                "<<" => left.checked_shl(u32::try_from(right).map_err(|_| Unreadable)?).ok_or(Unreadable)?,
                ">>" => left.checked_shr(u32::try_from(right).map_err(|_| Unreadable)?).ok_or(Unreadable)?,
                "+" => left.wrapping_add(right),
                "-" => left.wrapping_sub(right),
                "*" => left.wrapping_mul(right),
                "/" => left.checked_div(right).ok_or(Unreadable)?,
                _ => left.checked_rem(right).ok_or(Unreadable)?,
            };
        }
        Ok(left)
    }

    fn or(&mut self) -> Result<i64, Unreadable> {
        self.binary(0)
    }

    fn unary(&mut self) -> Result<i64, Unreadable> {
        let token = self.tokens.get(self.at).ok_or(Unreadable)?;
        self.at += 1;
        match &*token.text {
            "+" => self.unary(),
            "-" => Ok(self.unary()?.wrapping_neg()),
            "~" => Ok(!self.unary()?),
            "!" => Ok(i64::from(self.unary()? == 0)),
            "(" => {
                let value = self.or()?;
                if self.peek() != Some(")") {
                    return Err(Unreadable);
                }
                self.at += 1;
                Ok(value)
            }
            text if token.kind == Kind::Number => integer(text).ok_or(Unreadable),
            // An identifier no macro defines is zero, as in C.
            _ if token.kind == Kind::Ident => Ok(0),
            _ => Err(Unreadable),
        }
    }
}

/// A GLSL ES integer literal's value: decimal, octal or hexadecimal, with an optional `u`.
fn integer(text: &str) -> Option<i64> {
    let digits = text.strip_suffix(['u', 'U']).unwrap_or(text);
    if let Some(hex) = digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).ok()
    } else if digits.len() > 1 && digits.starts_with('0') {
        i64::from_str_radix(&digits[1..], 8).ok()
    } else {
        digits.parse().ok()
    }
}
