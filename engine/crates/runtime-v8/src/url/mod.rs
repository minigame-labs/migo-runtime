//! WPT-compliant URL parsing; reuse core's form codec and cppgc ownership.
use ada_url::Url;
use deno_core::{GarbageCollected, extension, op2, url::form_urlencoded, v8};
use deno_error::JsErrorBox;
use std::cell::RefCell;

pub struct ParsedUrl(RefCell<Url>);
type UrlParts = [String; 11];

// SAFETY: only an owned Rust URL, with no V8 or cppgc references to trace.
unsafe impl GarbageCollected for ParsedUrl {
    fn trace(&self, _: &mut v8::cppgc::Visitor) {}
    fn get_name(&self) -> &'static std::ffi::CStr {
        c"MigoURL"
    }
}

#[op2]
#[cppgc]
fn op_url_parse(
    #[string] input: &str,
    #[string] base: Option<String>,
) -> Result<ParsedUrl, JsErrorBox> {
    let url =
        Url::parse(input, base.as_deref()).map_err(|_| JsErrorBox::type_error("Invalid URL"))?;
    Ok(ParsedUrl(RefCell::new(url)))
}

fn parts(url: &Url) -> UrlParts {
    [
        url.href().into(),
        url.origin(),
        url.protocol().into(),
        url.username().into(),
        url.password().into(),
        url.host().into(),
        url.hostname().into(),
        url.port().into(),
        url.pathname().into(),
        url.search().into(),
        url.hash().into(),
    ]
}

#[op2]
#[serde]
fn op_url_parts(#[cppgc] url: &ParsedUrl) -> UrlParts {
    parts(&url.0.borrow())
}

#[op2]
#[serde]
fn op_url_set(
    #[cppgc] parsed: &ParsedUrl,
    field: u32,
    #[string] value: &str,
) -> Result<UrlParts, JsErrorBox> {
    let mut url = parsed.0.borrow_mut();
    // Public browser setters implement state overrides, including partial port
    // input, special schemes and invalid-setter no-ops. No handwritten grammar.
    match field {
        0 => url
            .set_href(value)
            .map_err(|_| JsErrorBox::type_error("Invalid URL"))?,
        2 => {
            let _ = url.set_protocol(value);
        }
        3 => {
            let _ = url.set_username(Some(value));
        }
        4 => {
            let _ = url.set_password(Some(value));
        }
        5 => {
            let _ = url.set_host(Some(value));
        }
        6 => {
            let _ = url.set_hostname(Some(value));
        }
        7 => {
            let _ = url.set_port(Some(value));
        }
        8 => {
            let _ = url.set_pathname(Some(value));
        }
        9 => url.set_search(Some(value)),
        10 => url.set_hash(Some(value)),
        _ => return Err(JsErrorBox::type_error("Invalid URL field")),
    }
    Ok(parts(&url))
}

#[op2]
#[serde]
fn op_url_search_parse(#[string] query: &str) -> Vec<[String; 2]> {
    form_urlencoded::parse(query.as_bytes())
        .map(|(key, value)| [key.into_owned(), value.into_owned()])
        .collect()
}

#[op2]
#[string]
fn op_url_search_serialize(#[serde] pairs: Vec<[String; 2]>) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().map(|pair| (&pair[0], &pair[1])))
        .finish()
}

extension!(host_v8_url,
    ops = [op_url_parse, op_url_parts, op_url_set, op_url_search_parse, op_url_search_serialize],
    esm = [dir "src/url", "00_url_backend.js", "03_url.js"],
);

pub(crate) fn url_extensions() -> Vec<deno_core::Extension> {
    vec![host_v8_url::init()]
}
pub(crate) fn url_lazy_extensions() -> Vec<deno_core::Extension> {
    vec![host_v8_url::lazy_init()]
}

#[cfg(test)]
mod tests;
