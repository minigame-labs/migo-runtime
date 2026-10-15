//! GLSL ES's preprocessing tokens, in logical lines.
//!
//! A comment is whitespace -- a block comment that spans lines does not end the line it starts on, as in C -- and a
//! backslash before a newline joins the two lines (ESSL 3.00). Every token is a slice of the source and knows where
//! it starts, so what is decided about a token can be applied to the source itself.

/// What a token is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Ident,
    Number,
    Punct,
    /// A character no token begins with; the driver reports it.
    Other,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Token<'a> {
    pub kind: Kind,
    pub text: &'a str,
    /// Byte offset of the token in the source.
    pub start: usize,
    /// Whitespace or a comment comes before it on its line: what tells `#define F(x)` from `#define F (x)`.
    pub spaced: bool,
}

impl Token<'_> {
    pub fn end(&self) -> usize {
        self.start + self.text.len()
    }

    pub fn is(&self, text: &str) -> bool {
        self.kind != Kind::Ident && self.kind != Kind::Number && self.text == text
    }
}

/// One logical line: a directive when its first token is `#`.
pub(super) struct Line<'a> {
    pub tokens: Vec<Token<'a>>,
}

impl Line<'_> {
    pub fn is_directive(&self) -> bool {
        self.tokens.first().is_some_and(|t| t.is("#"))
    }
}

const PUNCTUATORS: [&str; 22] = [
    "<<=", ">>=", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "^^", "+=", "-=", "*=", "/=", "%=",
    "&=", "^=", "|=", "##",
];

/// The source's logical lines, empty ones left out.
pub(super) fn lines(source: &str) -> Vec<Line<'_>> {
    let bytes = source.as_bytes();
    let mut lines = Vec::new();
    let mut current = Vec::new();
    let mut spaced = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'\n' | b'\r' => {
                // \r\n, \n\r, \r and \n each end one line.
                let pair = i + 1 < bytes.len() && (bytes[i + 1] == b'\n' || bytes[i + 1] == b'\r') && bytes[i + 1] != c;
                i += if pair { 2 } else { 1 };
                if !current.is_empty() {
                    lines.push(Line { tokens: std::mem::take(&mut current) });
                }
                spaced = false;
            }
            b' ' | b'\t' | 0x0b | 0x0c => {
                i += 1;
                spaced = true;
            }
            b'\\' if i + 1 < bytes.len() && (bytes[i + 1] == b'\n' || bytes[i + 1] == b'\r') => {
                let pair = i + 2 < bytes.len()
                    && (bytes[i + 2] == b'\n' || bytes[i + 2] == b'\r')
                    && bytes[i + 2] != bytes[i + 1];
                i += if pair { 3 } else { 2 };
                spaced = true;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                    i += 1;
                }
                spaced = true;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                spaced = true;
            }
            _ => {
                let start = i;
                let kind = if c == b'_' || c.is_ascii_alphabetic() {
                    while i < bytes.len() && (bytes[i] == b'_' || bytes[i].is_ascii_alphanumeric()) {
                        i += 1;
                    }
                    Kind::Ident
                } else if c.is_ascii_digit() || (c == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) {
                    i += 1;
                    while i < bytes.len() {
                        let d = bytes[i];
                        if (d == b'+' || d == b'-') && matches!(bytes[i - 1], b'e' | b'E') && !is_hex(&source[start..i])
                        {
                            i += 1;
                        } else if d == b'_' || d == b'.' || d.is_ascii_alphanumeric() {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    Kind::Number
                } else if let Some(p) = PUNCTUATORS.iter().find(|p| source[i..].starts_with(*p)) {
                    i += p.len();
                    Kind::Punct
                } else if b"(){}[].,;:?+-*/%<>=!~&|^#".contains(&c) {
                    i += 1;
                    Kind::Punct
                } else {
                    // One character, whole: the source is UTF-8.
                    i += source[i..].chars().next().map_or(1, char::len_utf8);
                    Kind::Other
                };
                current.push(Token {
                    kind,
                    text: &source[start..i],
                    start,
                    spaced,
                });
                spaced = false;
            }
        }
    }
    if !current.is_empty() {
        lines.push(Line { tokens: current });
    }
    lines
}

fn is_hex(number: &str) -> bool {
    number.starts_with("0x") || number.starts_with("0X")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(source: &str) -> Vec<Vec<&str>> {
        lines(source)
            .iter()
            .map(|l| l.tokens.iter().map(|t| t.text).collect())
            .collect()
    }

    #[test]
    fn tokens_are_slices_of_the_source_in_lines() {
        assert_eq!(
            texts("vec3 a = vec3(1.5e-3, 0x1F, .5);\n#define X(y) y<<=2\n"),
            vec![
                vec!["vec3", "a", "=", "vec3", "(", "1.5e-3", ",", "0x1F", ",", ".5", ")", ";"],
                vec!["#", "define", "X", "(", "y", ")", "y", "<<=", "2"],
            ]
        );
    }

    #[test]
    fn comments_are_whitespace_and_a_block_comment_does_not_end_its_line() {
        let all = lines("a /* one\ntwo */ b // c\nd");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].tokens.iter().map(|t| t.text).collect::<Vec<_>>(), ["a", "b"]);
        assert!(all[0].tokens[1].spaced);
        assert_eq!(all[1].tokens[0].text, "d");
    }

    #[test]
    fn a_backslash_newline_joins_lines() {
        let all = lines("#define A 1 \\\n + 2\nB");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].tokens.iter().map(|t| t.text).collect::<Vec<_>>(), ["#", "define", "A", "1", "+", "2"]);
        assert_eq!(all[1].tokens[0].text, "B");
    }

    #[test]
    fn a_function_like_macro_is_told_by_the_space_before_its_parenthesis() {
        let all = lines("#define F(x) x\n#define G (x)");
        assert!(!all[0].tokens[3].spaced);
        assert!(all[1].tokens[3].spaced);
    }

    #[test]
    fn offsets_point_into_the_source() {
        let source = "  float x;";
        let all = lines(source);
        let x = all[0].tokens[1];
        assert_eq!(&source[x.start..x.end()], "x");
    }
}
