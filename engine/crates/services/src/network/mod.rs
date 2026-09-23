//! The network the engine reaches, as the host reaches it.
//!
//! Content's `fetch`, its sockets and its prefetches are made by the host --
//! never by WebContent's own `fetch` -- because the engine's network API has
//! no CORS, carries the host's TLS and domain policy, and has sockets the web
//! platform does not have. So the policy that decides what may be reached, and
//! the address rules underneath it, live here: one set of rules for both
//! executions, callable without a script runtime.

pub mod address_filter;
pub mod client;
pub mod dns_cache;
pub mod fetch;
pub mod gate;
pub mod resources;
pub mod websocket;
