//! `multipart/form-data` (RFC 7578), as `uploadFile` sends it: a file part, then the form's text fields.
//!
//! Field names and file names are written as the HTML standard's form encoder writes them -- UTF-8, with only `"`,
//! CR and LF escaped (as `%22`, `%0D`, `%0A`) -- which is what a server reading a browser's upload expects.

use std::hash::{BuildHasher, Hasher};
use std::io;

use bytes::Bytes;
use futures::{Stream, StreamExt, stream};

use super::body::{Body, ByteStream};

/// A form to send.
pub struct Form {
    boundary: String,
    parts: Vec<(String, Part)>,
}

/// One field of a form.
pub struct Part {
    content: Content,
    file_name: Option<String>,
    mime: Option<String>,
}

enum Content {
    Text(String),
    Stream {
        stream: ByteStream,
        length: Option<u64>,
    },
}

impl Part {
    pub fn text(value: impl Into<String>) -> Self {
        Self {
            content: Content::Text(value.into()),
            file_name: None,
            mime: None,
        }
    }

    /// A part read from `stream` as the form is sent; `length` when it is known.
    pub fn stream(
        stream: impl Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
        length: Option<u64>,
    ) -> Self {
        Self {
            content: Content::Stream {
                stream: Box::pin(stream),
                length,
            },
            file_name: None,
            mime: None,
        }
    }

    pub fn file_name(mut self, name: impl Into<String>) -> Self {
        self.file_name = Some(name.into());
        self
    }

    pub fn mime(mut self, mime: impl Into<String>) -> Self {
        self.mime = Some(mime.into());
        self
    }
}

impl Default for Form {
    fn default() -> Self {
        Self::new()
    }
}

impl Form {
    pub fn new() -> Self {
        // A boundary no part's content will contain, and no one outside can predict: `RandomState`'s keys are seeded
        // from the operating system's randomness once per thread and differ for every instance, so hashing nothing
        // under four of them gives four unpredictable words.
        let random = || {
            std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish()
        };
        Self {
            boundary: format!(
                "{:016x}-{:016x}-{:016x}-{:016x}",
                random(),
                random(),
                random(),
                random()
            ),
            parts: Vec::new(),
        }
    }

    pub fn part(mut self, name: impl Into<String>, part: Part) -> Self {
        self.parts.push((name.into(), part));
        self
    }

    pub fn text(self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.part(name, Part::text(value))
    }

    pub fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }

    /// The encoded form: chunked unless every part's length is known.
    pub fn into_body(self) -> Body {
        let mut length = Some(0u64);
        let mut pieces: Vec<ByteStream> = Vec::with_capacity(self.parts.len() * 3 + 1);
        for (name, part) in self.parts {
            let head = Bytes::from(part_head(&self.boundary, &name, &part));
            length = length.map(|total| total + head.len() as u64);
            pieces.push(Box::pin(stream::once(async move { Ok(head) })));
            match part.content {
                Content::Text(text) => {
                    length = length.map(|total| total + text.len() as u64);
                    let text = Bytes::from(text);
                    pieces.push(Box::pin(stream::once(async move { Ok(text) })));
                }
                Content::Stream {
                    stream,
                    length: known,
                } => {
                    length = length.zip(known).map(|(total, part)| total + part);
                    pieces.push(stream);
                }
            }
            length = length.map(|total| total + 2);
            pieces.push(Box::pin(stream::once(async {
                Ok(Bytes::from_static(b"\r\n"))
            })));
        }
        let tail = Bytes::from(format!("--{}--\r\n", self.boundary));
        length = length.map(|total| total + tail.len() as u64);
        pieces.push(Box::pin(stream::once(async move { Ok(tail) })));
        Body::wrap_stream(stream::iter(pieces).flatten(), length)
    }
}

fn part_head(boundary: &str, name: &str, part: &Part) -> String {
    let mut head = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"",
        escape(name)
    );
    if let Some(file_name) = &part.file_name {
        head.push_str(&format!("; filename=\"{}\"", escape(file_name)));
    }
    head.push_str("\r\n");
    if let Some(mime) = &part.mime {
        head.push_str(&format!("Content-Type: {mime}\r\n"));
    }
    head.push_str("\r\n");
    head
}

fn escape(value: &str) -> String {
    value
        .replace('"', "%22")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn encoded(form: Form) -> (String, Option<u64>) {
        let body = form.into_body();
        let declared = body.length();
        let progress = std::sync::Arc::new(super::super::body::SendProgress::default());
        let bytes = body.sending(progress).collect().await.unwrap().to_bytes();
        (String::from_utf8(bytes.to_vec()).unwrap(), declared)
    }

    /// The parts in order, each with its disposition, the file part with its name and type, closed by the final
    /// boundary -- and a length declared that is the length sent.
    #[tokio::test]
    async fn a_form_is_encoded_part_by_part_with_its_length() {
        let form = Form::new();
        let boundary = form.boundary.clone();
        let file = stream::iter([
            Ok(Bytes::from_static(b"PNG")),
            Ok(Bytes::from_static(b"DATA")),
        ]);
        let form = form
            .part(
                "file",
                Part::stream(file, Some(7))
                    .file_name("a.png")
                    .mime("image/png"),
            )
            .text("user", "42");
        let (text, declared) = encoded(form).await;
        let expected = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.png\"\r\nContent-Type: image/png\r\n\r\nPNGDATA\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"user\"\r\n\r\n42\r\n--{boundary}--\r\n"
        );
        assert_eq!(text, expected);
        assert_eq!(declared, Some(expected.len() as u64));
    }

    /// A file of unknown length makes the whole form's length unknown: it is sent chunked.
    #[tokio::test]
    async fn a_part_of_unknown_length_leaves_the_length_undeclared() {
        let file = stream::iter([Ok(Bytes::from_static(b"x"))]);
        let (_, declared) = encoded(Form::new().part("file", Part::stream(file, None))).await;
        assert_eq!(declared, None);
    }

    /// Names are UTF-8 as written, with only the three characters that would end or break the header escaped.
    #[tokio::test]
    async fn names_are_escaped_as_the_html_form_encoder_escapes_them() {
        let (text, _) =
            encoded(Form::new().part("f", Part::text("v").file_name("\u{6587}\"a\r\nb.txt"))).await;
        assert!(
            text.contains("filename=\"\u{6587}%22a%0D%0Ab.txt\""),
            "{text}"
        );
    }

    /// Two forms do not share a boundary.
    #[test]
    fn every_form_draws_its_own_boundary() {
        assert_ne!(Form::new().boundary, Form::new().boundary);
    }
}
