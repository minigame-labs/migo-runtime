use deno_core::extension;

use crate::network::fetch::{
    op_fetch, op_fetch_send, op_fetch_upload, op_fetch_upload_cancel_handle,
};
use crate::network::prefetch::{op_prefetch_assets, op_prefetch_dns};
use crate::network::tcp_socket::{op_tcp_close, op_tcp_connect, op_tcp_next_event, op_tcp_write};
use crate::network::udp_socket::{
    op_udp_bind, op_udp_close, op_udp_connect, op_udp_next_event, op_udp_send, op_udp_set_ttl,
};
use crate::network::websocket::{op_ws_close, op_ws_create, op_ws_next_event, op_ws_send};

pub(crate) mod fetch;
pub(crate) mod gate;
mod prefetch;
mod tcp_socket;
mod udp_socket;
mod websocket;

#[derive(Clone)]
struct Options {
    pub user_agent: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            user_agent: "migo".to_string(),
        }
    }
}

extension!(host_v8_network,
  deps = [host_v8_console, host_v8_web, host_v8_base],
  ops = [
    op_fetch, op_fetch_send, op_fetch_upload, op_fetch_upload_cancel_handle,
    op_prefetch_dns, op_prefetch_assets,
    op_ws_create, op_ws_next_event, op_ws_send, op_ws_close,
    op_tcp_connect, op_tcp_next_event, op_tcp_write, op_tcp_close,
    op_udp_bind, op_udp_connect, op_udp_send, op_udp_set_ttl, op_udp_next_event, op_udp_close,
  ],
  esm = [
     dir "src/network",
     "00_binary.js",
     "01_header.js",
     "02_response.js",
     "03_task.js",
     "04_request.js",
     "05_download.js",
     "06_upload.js",
     "07_websocket.js",
     "08_tcp_socket.js",
     "09_udp_socket.js",
     "10_prefetch.js",
  ],
  options = {
    options: Options,
  },
  state = |state, options| {
    state.put::<Options>(options.options);
    // This runtime's network resources live in the service, one table per
    // runtime, so a request, its cancel handle and its response body are the
    // same objects both executions hold (see `fetch::NetworkResources`).
    state.put::<fetch::NetworkResources>(fetch::NetworkResources(std::sync::Arc::new(
        migo_services::network::resources::ResourceTable::new(),
    )));
  },
);

pub(crate) fn network_extensions() -> Vec<deno_core::Extension> {
    vec![host_v8_network::init(Default::default())]
}

pub(crate) fn network_lazy_extensions() -> Vec<deno_core::Extension> {
    vec![host_v8_network::lazy_init()]
}

/// Create extension args with default Options.
/// This wrapper avoids exposing the private `Options` type outside this module.
pub(crate) fn network_extension_args() -> deno_core::ExtensionArguments {
    host_v8_network::args(Default::default())
}
