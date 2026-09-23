//! The network service ops, as the external session answers them.
//!
//! Content's `fetch` is made here, by the host, for the reason the op boundary
//! gives: the engine's network API has no CORS, carries the host's TLS and
//! domain policy, and reaches hosts WebContent's own `fetch` would refuse. So
//! the producer calls `op_fetch` and `op_fetch_send` over the service stream
//! and this runs the same `migo_services::network` code the embedded op calls.
//!
//! # The handles content holds
//!
//! A request, the handle that aborts it and the response body are resources of
//! the session (`migo_services::network::resources`), named by plain ids. The
//! embedded runtime wraps each id in a deno resource so `core.read` finds it in
//! process; here the ids *are* what content holds, and the producer's
//! `core.read`, `core.close` and `core.tryClose` are the three ops below --
//! which is why they are service ops at all. Nothing else in the engine's
//! JavaScript uses deno's resource table.
//!
//! A body is read, never buffered whole: `op_core_read` answers at most what
//! the caller asked for, and the empty answer is the end of the body. That is
//! the same contract `ReadableStream` pulls the embedded body with, so the
//! engine's `Response` code is identical on both executions.

use std::sync::Arc;

use frame_wire::value::OwnedValue;
use futures::future::BoxFuture;
use migo_io::scheduler::IoScheduler;
use migo_services::ServiceError;
use migo_services::network::client::{PolicyHttpClient, create_policy_http_client};
use migo_services::network::fetch::{self as fetch_service, FetchEnv, RequestBody};
use migo_services::network::gate::GateKind;
use migo_services::network::resources::{ResourceId, ResourceTable};
use migo_services::network::sockets::AddrMeta;
use migo_services::network::tcp::{self as tcp, TcpEvent};
use migo_services::network::udp::{self as udp, UdpEvent};
use migo_services::network::upload::{self as upload, UploadEnv, UploadRequest};
use migo_services::network::websocket::{self as websocket, WsEvent};
use parking_lot::Mutex;
use shared::op_state::NetworkPolicy;

use super::service_args::{
    boolean, exactly, not_a, optional_bytes, optional_string, string, strings, u32_of, wrong_type,
};
use super::service_ops::id;

/// The user agent every request this session makes carries. The embedded
/// runtime builds its own from the same constant, so a server sees one engine
/// whichever execution content runs in.
const USER_AGENT: &str = "migo";

/// The session's network: the policy every request is held to, the clients that
/// policy configured, and the resources content holds ids into.
pub(crate) struct NetworkBinding {
    policy: NetworkPolicy,
    /// Whether the app is in the background, which is what a socket's read
    /// throttles on: the connection stays up and delivery is deferred, rather
    /// than a backgrounded game spinning on messages nobody will see.
    backgrounded: Arc<std::sync::atomic::AtomicBool>,
    resources: Arc<ResourceTable>,
    /// The session's Tokio runtime.
    ///
    /// A synchronous op runs on the thread that made the call -- the host's
    /// synchronous endpoint -- and building a request needs a reactor: the
    /// client's connection pool arms timers, and a `data:` answer is still a
    /// body a reader will be polled on. The embedded runtime never has to say
    /// this because its ops run on the thread its runtime drives; here the
    /// context is entered explicitly for the length of the call.
    runtime: tokio::runtime::Handle,
    /// One client per protocol version, built on first use: construction is TLS
    /// setup, and a game that never fetches must not pay it. Held behind a lock
    /// rather than a `OnceLock` because building can fail, and a failure must
    /// be answered rather than remembered.
    http1: Mutex<Option<PolicyHttpClient>>,
    http2: Mutex<Option<PolicyHttpClient>>,
}

impl NetworkBinding {
    pub(crate) fn new(
        policy: NetworkPolicy,
        backgrounded: Arc<std::sync::atomic::AtomicBool>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            policy,
            backgrounded,
            resources: Arc::new(ResourceTable::new()),
            runtime,
            http1: Mutex::new(None),
            http2: Mutex::new(None),
        }
    }

    /// The client for `enable_http2`, built once.
    fn client(&self, enable_http2: bool) -> Result<PolicyHttpClient, ServiceError> {
        let slot = if enable_http2 { &self.http2 } else { &self.http1 };
        let mut slot = slot.lock();
        if let Some(client) = slot.as_ref() {
            return Ok(client.clone());
        }
        let client = create_policy_http_client(
            USER_AGENT,
            enable_http2,
            &self.policy,
            GateKind::FetchRedirect,
            "fetch",
        )
        .map_err(|error| ServiceError::generic(format!("Failed to create HTTP client: {error}")))?;
        *slot = Some(client.clone());
        Ok(client)
    }

    /// This session's policy and the client it configured, for a fetch that is
    /// not an op: an `http(s)://` image source, which is held to exactly what
    /// `fetch()` is.
    pub(crate) fn image_client(
        &self,
    ) -> Result<(NetworkPolicy, PolicyHttpClient), ServiceError> {
        let _in_the_session_s_runtime = self.runtime.enter();
        Ok((self.policy.clone(), self.client(false)?))
    }

    /// The table content's ids name. Public for the session's teardown, which
    /// is the only other thing that touches it.
    pub(crate) fn resources(&self) -> &Arc<ResourceTable> {
        &self.resources
    }
}

/// Where an upload reads the file content named: the mounted package's
/// sandbox, or -- before content is mounted -- nowhere.
pub(crate) struct UploadSources {
    pub(crate) vfs: Option<Arc<shared::vfs::VirtualFS>>,
    pub(crate) mount_table: Option<Arc<shared::vfs::MountTable>>,
}

/// Whether `op` is a synchronous network op this module answers.
pub(crate) fn is_sync(op: u32) -> bool {
    matches!(
        op,
        id::op_fetch | id::op_udp_bind | id::op_fetch_upload_cancel_handle
    )
}

/// Whether `op` is an awaited network op this module answers.
pub(crate) fn is_async(op: u32) -> bool {
    matches!(
        op,
        id::op_fetch_send
            | id::core_read
            | id::op_ws_create
            | id::op_ws_next_event
            | id::op_ws_send
            | id::op_ws_close
            | id::op_tcp_connect
            | id::op_tcp_next_event
            | id::op_tcp_write
            | id::op_udp_connect
            | id::op_udp_send
            | id::op_udp_next_event
            | id::op_fetch_upload
    )
}

/// Whether `op` is a network command this module answers.
pub(crate) fn is_command(op: u32) -> bool {
    matches!(
        op,
        id::core_close
            | id::core_try_close
            | id::op_prefetch_dns
            | id::op_tcp_close
            | id::op_udp_close
            | id::op_udp_set_ttl
    )
}

/// Run a synchronous network op.
pub(crate) fn call_sync(
    network: &NetworkBinding,
    scheduler: &IoScheduler,
    op: u32,
    args: Vec<OwnedValue>,
) -> Result<OwnedValue, ServiceError> {
    let _in_the_session_s_runtime = network.runtime.enter();
    match op {
        id::op_fetch => {
            let [method, url, headers, client_rid, has_body, data, resource, timeout, enable_http2, enable_cache] =
                exactly(op, args)?;
            // The engine's JavaScript passes neither: it builds no
            // `HttpClient` of its own, and a request body is bytes it has in
            // hand rather than a stream resource. A producer that started
            // passing one is refused rather than silently ignored.
            if !matches!(client_rid, OwnedValue::Null) {
                return Err(ServiceError::generic(
                    "op_fetch takes no client on this lane",
                ));
            }
            if !matches!(resource, OwnedValue::Null) {
                return Err(ServiceError::generic(
                    "op_fetch takes no body resource on this lane",
                ));
            }
            let method = match method {
                OwnedValue::Bytes(bytes) => bytes,
                OwnedValue::Str(text) => text.into_bytes(),
                other => return Err(wrong_type(op, 0, "byte string", &other)),
            };
            let url = string(op, 1, url)?;
            let headers = header_pairs(op, 2, headers)?;
            let has_body = boolean(op, 4, has_body)?;
            let data = optional_bytes(op, 5, data)?;
            let timeout = u32_of(op, 7, timeout)?;
            let enable_http2 = boolean(op, 8, enable_http2)?;
            let enable_cache = boolean(op, 9, enable_cache)?;

            let client = network.client(enable_http2)?;
            let body = match (has_body, data.as_deref()) {
                (true, Some(bytes)) => RequestBody::Bytes(bytes),
                _ => RequestBody::None,
            };
            let handles = fetch_service::fetch(
                FetchEnv {
                    policy: &network.policy,
                    client: &client,
                    resources: &network.resources,
                    pools: scheduler.pools(),
                    // No client resource exists on this lane, so no request
                    // carries the permission to set its own `Host`.
                    allow_host: false,
                },
                &method,
                &url,
                &headers,
                body,
                timeout,
                enable_cache,
            )?;
            // `FetchReturn`, in its field order: requestRid, cancelHandleRid.
            Ok(OwnedValue::Array(vec![
                OwnedValue::U32(handles.request_rid),
                match handles.cancel_handle_rid {
                    Some(id) => OwnedValue::U32(id),
                    None => OwnedValue::Null,
                },
            ]))
        }
        id::op_udp_bind => {
            let [port, socket_type] = exactly(op, args)?;
            let port = u32_of(op, 0, port)?;
            let socket_type = string(op, 1, socket_type)?;
            // `UdpBindResult`, in its field order: rid, port, address, family.
            udp::bind(&network.resources, port, &socket_type).map(|bound| {
                OwnedValue::Array(vec![
                    OwnedValue::U32(bound.rid),
                    OwnedValue::U32(u32::from(bound.local.port)),
                    OwnedValue::Str(bound.local.address.to_string()),
                    OwnedValue::Str(bound.local.family.to_string()),
                ])
            })
        }
        id::op_fetch_upload_cancel_handle => {
            let [] = exactly(op, args)?;
            // Synchronous because the handle names a host resource and content
            // is given it before it awaits the upload.
            Ok(OwnedValue::U32(upload::cancel_handle(&network.resources)))
        }
        other => Err(not_a(other, "synchronous network")),
    }
}

/// Start an awaited network op.
pub(crate) fn call_async(
    network: &NetworkBinding,
    scheduler: &IoScheduler,
    sources: UploadSources,
    op: u32,
    args: Vec<OwnedValue>,
) -> Result<BoxFuture<'static, Result<OwnedValue, ServiceError>>, ServiceError> {
    let resources = Arc::clone(&network.resources);
    Ok(match op {
        id::op_fetch_send => {
            let [rid] = exactly(op, args)?;
            let rid: ResourceId = u32_of(op, 0, rid)?;
            let pools = scheduler.pools().clone();
            Box::pin(async move {
                fetch_service::fetch_send(&resources, &pools, rid)
                    .await
                    .map(fetch_response)
            })
        }
        id::core_read => {
            let [rid, limit] = exactly(op, args)?;
            let rid: ResourceId = u32_of(op, 0, rid)?;
            let limit = u32_of(op, 1, limit)? as usize;
            Box::pin(async move {
                fetch_service::read(&resources, rid, limit)
                    .await
                    .map(|bytes| OwnedValue::Bytes(bytes.to_vec()))
            })
        }
        id::op_ws_create => {
            let [url, protocols, headers, timeout_ms] = exactly(op, args)?;
            let url = string(op, 0, url)?;
            let protocols = strings(op, 1, protocols)?;
            let headers = header_strings(op, 2, headers)?;
            let timeout_ms = match timeout_ms {
                OwnedValue::Null => None,
                other => Some(u32_of(op, 3, other)?),
            };
            let policy = network.policy.clone();
            Box::pin(async move {
                websocket::create(&policy, &resources, &url, &protocols, &headers, timeout_ms)
                    .await
                    // `WsCreateResult`, in its field order: rid, protocol,
                    // extensions.
                    .map(|handshake| {
                        OwnedValue::Array(vec![
                            OwnedValue::U32(handshake.rid),
                            OwnedValue::Str(handshake.protocol),
                            OwnedValue::Str(handshake.extensions),
                        ])
                    })
            })
        }
        id::op_ws_next_event => {
            let [rid] = exactly(op, args)?;
            let rid: ResourceId = u32_of(op, 0, rid)?;
            let backgrounded = Arc::clone(&network.backgrounded);
            Box::pin(async move {
                websocket::next_event(&resources, rid, &backgrounded)
                    .await
                    .map(ws_event)
            })
        }
        id::op_ws_send => {
            let [rid, text, bytes] = exactly(op, args)?;
            let rid: ResourceId = u32_of(op, 0, rid)?;
            let text = optional_string(op, 1, text)?;
            let bytes = optional_bytes(op, 2, bytes)?;
            Box::pin(async move {
                websocket::send(&resources, rid, text, bytes)
                    .await
                    .map(|()| OwnedValue::Null)
            })
        }
        id::op_ws_close => {
            let [rid, code, reason] = exactly(op, args)?;
            let rid: ResourceId = u32_of(op, 0, rid)?;
            // `#[smi] u16`: the low sixteen bits, as deno_core narrows it.
            let code = u32_of(op, 1, code)? as u16;
            let reason = string(op, 2, reason)?;
            Box::pin(async move {
                websocket::close(&resources, rid, code, reason)
                    .await
                    .map(|()| OwnedValue::Null)
            })
        }
        id::op_tcp_connect => {
            let [address, port, timeout_secs] = exactly(op, args)?;
            let address = string(op, 0, address)?;
            let port = u32_of(op, 1, port)?;
            let timeout_secs = u32_of(op, 2, timeout_secs)?;
            let policy = network.policy.clone();
            Box::pin(async move {
                tcp::connect(&policy, &resources, &address, port, timeout_secs)
                    .await
                    // `TcpConnectResult`, in its field order.
                    .map(|connected| {
                        let mut fields = vec![OwnedValue::U32(connected.rid)];
                        fields.extend(endpoint(&connected.endpoints.remote));
                        fields.extend(endpoint(&connected.endpoints.local));
                        OwnedValue::Array(fields)
                    })
            })
        }
        id::op_tcp_next_event => {
            let [rid] = exactly(op, args)?;
            let rid: ResourceId = u32_of(op, 0, rid)?;
            let backgrounded = Arc::clone(&network.backgrounded);
            Box::pin(async move {
                tcp::next_event(&resources, rid, &backgrounded)
                    .await
                    .map(tcp_event)
            })
        }
        id::op_tcp_write => {
            let [rid, text, bytes] = exactly(op, args)?;
            let rid: ResourceId = u32_of(op, 0, rid)?;
            let text = optional_string(op, 1, text)?;
            let bytes = optional_bytes(op, 2, bytes)?;
            let pools = scheduler.pools().clone();
            Box::pin(async move {
                tcp::write(&resources, &pools, rid, text, bytes)
                    .await
                    .map(|()| OwnedValue::Null)
            })
        }
        id::op_udp_connect => {
            let [rid, address, port] = exactly(op, args)?;
            let rid: ResourceId = u32_of(op, 0, rid)?;
            let address = string(op, 1, address)?;
            let port = u32_of(op, 2, port)?;
            let policy = network.policy.clone();
            Box::pin(async move {
                udp::connect(&policy, &resources, rid, &address, port)
                    .await
                    .map(|()| OwnedValue::Null)
            })
        }
        id::op_udp_send => {
            let [rid, address, port, text, bytes, offset, length, set_broadcast] =
                exactly(op, args)?;
            let rid: ResourceId = u32_of(op, 0, rid)?;
            let address = string(op, 1, address)?;
            let port = u32_of(op, 2, port)?;
            let text = optional_string(op, 3, text)?;
            let bytes = optional_bytes(op, 4, bytes)?;
            let offset = u32_of(op, 5, offset)?;
            let length = u32_of(op, 6, length)?;
            let set_broadcast = boolean(op, 7, set_broadcast)?;
            let policy = network.policy.clone();
            Box::pin(async move {
                udp::send(
                    &policy,
                    &resources,
                    rid,
                    &address,
                    port,
                    text,
                    bytes,
                    offset,
                    length,
                    set_broadcast,
                )
                .await
                .map(|()| OwnedValue::Null)
            })
        }
        id::op_udp_next_event => {
            let [rid] = exactly(op, args)?;
            let rid: ResourceId = u32_of(op, 0, rid)?;
            let backgrounded = Arc::clone(&network.backgrounded);
            Box::pin(async move {
                udp::next_event(&resources, rid, &backgrounded)
                    .await
                    .map(udp_event)
            })
        }
        id::op_fetch_upload => {
            let [cancel_rid, url, file_path, name, filename, headers, form_data, timeout, _enable_http2] =
                exactly(op, args)?;
            let request = UploadRequest {
                cancel_rid: u32_of(op, 0, cancel_rid)?,
                url: string(op, 1, url)?,
                file_path: string(op, 2, file_path)?,
                name: string(op, 3, name)?,
                filename: string(op, 4, filename)?,
                headers: header_pairs(op, 5, headers)?,
                form_data: header_strings(op, 6, form_data)?,
                timeout_ms: u32_of(op, 7, timeout)?,
            };
            // HTTP/2 is the client's, and this lane has one client per version;
            // an upload takes the 1.1 one, as a multipart POST always has.
            let client = network.client(false)?;
            let policy = network.policy.clone();
            let content = sources;
            Box::pin(async move {
                upload::upload(
                    UploadEnv {
                        policy: &policy,
                        client: &client,
                        resources: &resources,
                        vfs: content.vfs.as_deref(),
                        mount_table: content.mount_table.as_deref(),
                    },
                    request,
                )
                .await
                .map(upload_answer)
            })
        }
        other => return Err(not_a(other, "awaited network")),
    })
}

/// Apply a network command: the two closes, which answer nothing.
pub(crate) fn command(
    network: &NetworkBinding,
    op: u32,
    args: Vec<OwnedValue>,
) -> Result<(), ServiceError> {
    let _in_the_session_s_runtime = network.runtime.enter();
    match op {
        id::core_close | id::core_try_close => {
            let [rid] = exactly(op, args)?;
            let rid: ResourceId = u32_of(op, 0, rid)?;
            let found = network.resources.close(rid);
            // `close` refuses an id that names nothing, as deno's does;
            // `tryClose` is the form content calls when it does not care.
            if !found && op == id::core_close {
                return Err(ServiceError::generic(format!("resource {rid} is not open")));
            }
            Ok(())
        }
        id::op_prefetch_dns => {
            let [hosts_json] = exactly(op, args)?;
            migo_services::network::dns_cache::prefetch_dns(
                &network.policy,
                &string(op, 0, hosts_json)?,
            )
        }
        id::op_tcp_close => {
            let [rid] = exactly(op, args)?;
            tcp::close(&network.resources, u32_of(op, 0, rid)?)
        }
        id::op_udp_close => {
            let [rid] = exactly(op, args)?;
            udp::close(&network.resources, u32_of(op, 0, rid)?)
        }
        id::op_udp_set_ttl => {
            let [rid, ttl] = exactly(op, args)?;
            udp::set_ttl(
                &network.resources,
                u32_of(op, 0, rid)?,
                u32_of(op, 1, ttl)?,
            )
        }
        other => Err(not_a(other, "network command")),
    }
}

/// `FetchResponse`, in its field order: status, statusText, headers, url,
/// responseRid, contentLength, remoteAddrIp, remoteAddrPort, error.
fn fetch_response(answer: fetch_service::FetchAnswer) -> OwnedValue {
    OwnedValue::Array(vec![
        OwnedValue::U32(u32::from(answer.status)),
        OwnedValue::Str(answer.status_text),
        OwnedValue::Array(
            answer
                .headers
                .into_iter()
                .map(|(name, value)| {
                    OwnedValue::Array(vec![OwnedValue::Bytes(name), OwnedValue::Bytes(value)])
                })
                .collect(),
        ),
        OwnedValue::Str(answer.url),
        OwnedValue::U32(answer.response_rid),
        optional_u64(answer.content_length),
        match answer.remote_addr_ip {
            Some(ip) => OwnedValue::Str(ip),
            None => OwnedValue::Null,
        },
        match answer.remote_addr_port {
            Some(port) => OwnedValue::U32(u32::from(port)),
            None => OwnedValue::Null,
        },
        match answer.error {
            Some(error) => OwnedValue::Str(error),
            None => OwnedValue::Null,
        },
    ])
}

/// A socket event, tagged as the engine's facade reads it: the kind, then the
/// fields that kind carries. The producer rebuilds the object from this rather
/// than being sent the absent fields as nulls.
fn ws_event(event: WsEvent) -> OwnedValue {
    let tagged = |tag: u32, fields: Vec<OwnedValue>| {
        let mut all = vec![OwnedValue::U32(tag)];
        all.extend(fields);
        OwnedValue::Array(all)
    };
    match event {
        WsEvent::Text(text) => tagged(WS_EVENT_TEXT, vec![OwnedValue::Str(text)]),
        WsEvent::Binary(data) => tagged(WS_EVENT_BINARY, vec![OwnedValue::Bytes(data)]),
        WsEvent::Error(message) => tagged(WS_EVENT_ERROR, vec![OwnedValue::Str(message)]),
        WsEvent::Close { code, reason } => tagged(
            WS_EVENT_CLOSE,
            vec![OwnedValue::U32(u32::from(code)), OwnedValue::Str(reason)],
        ),
    }
}

/// `FetchUploadResult`, in its field order: data, statusCode, headers,
/// totalBytesSent, error.
fn upload_answer(answer: upload::UploadAnswer) -> OwnedValue {
    OwnedValue::Array(vec![
        OwnedValue::Str(answer.data),
        OwnedValue::U32(u32::from(answer.status_code)),
        OwnedValue::Array(
            answer
                .headers
                .into_iter()
                .map(|(name, value)| {
                    OwnedValue::Array(vec![OwnedValue::Bytes(name), OwnedValue::Bytes(value)])
                })
                .collect(),
        ),
        OwnedValue::U64(answer.total_bytes_sent),
        match answer.error {
            Some(error) => OwnedValue::Str(error),
            None => OwnedValue::Null,
        },
    ])
}

/// One end of a socket, in the order every socket answer carries it: address,
/// family, port.
fn endpoint(meta: &AddrMeta) -> [OwnedValue; 3] {
    [
        OwnedValue::Str(meta.address.to_string()),
        OwnedValue::Str(meta.family.to_string()),
        OwnedValue::U32(u32::from(meta.port)),
    ]
}

/// A TCP event, tagged as a socket event is: the kind, then its fields.
fn tcp_event(event: TcpEvent) -> OwnedValue {
    match event {
        TcpEvent::Message { data, endpoints } => {
            let mut fields = vec![
                OwnedValue::U32(SOCKET_EVENT_MESSAGE),
                OwnedValue::Bytes(data.into_vec()),
            ];
            fields.extend(endpoint(&endpoints.remote));
            fields.extend(endpoint(&endpoints.local));
            OwnedValue::Array(fields)
        }
        TcpEvent::Error(message) => OwnedValue::Array(vec![
            OwnedValue::U32(SOCKET_EVENT_ERROR),
            OwnedValue::Str(message),
        ]),
        TcpEvent::Close => OwnedValue::Array(vec![OwnedValue::U32(SOCKET_EVENT_CLOSE)]),
    }
}

/// A UDP event. A datagram carries its size beside its bytes because the
/// facade reports it, and a socket has no close event of its own: closing is
/// content's own call.
fn udp_event(event: UdpEvent) -> OwnedValue {
    match event {
        UdpEvent::Message {
            data,
            remote,
            local,
        } => {
            let size = data.len() as u32;
            let mut fields = vec![
                OwnedValue::U32(SOCKET_EVENT_MESSAGE),
                OwnedValue::Bytes(data.into_vec()),
                OwnedValue::U32(size),
            ];
            fields.extend(endpoint(&remote));
            fields.extend(endpoint(&local));
            OwnedValue::Array(fields)
        }
        UdpEvent::Error(message) => OwnedValue::Array(vec![
            OwnedValue::U32(SOCKET_EVENT_ERROR),
            OwnedValue::Str(message),
        ]),
    }
}

/// The tags a raw socket's event travels under, shared by TCP and UDP.
const SOCKET_EVENT_MESSAGE: u32 = 0;
const SOCKET_EVENT_ERROR: u32 = 1;
const SOCKET_EVENT_CLOSE: u32 = 2;

/// The tags a socket event travels under. The producer's `network.mjs` has the
/// same four; they are a wire detail of this op, not a contract number.
const WS_EVENT_TEXT: u32 = 0;
const WS_EVENT_BINARY: u32 = 1;
const WS_EVENT_ERROR: u32 = 2;
const WS_EVENT_CLOSE: u32 = 3;

/// `Vec<(String, String)>`: a socket's extra headers, as pairs of strings.
fn header_strings(
    op: u32,
    index: usize,
    value: OwnedValue,
) -> Result<Vec<(String, String)>, ServiceError> {
    let list = match value {
        OwnedValue::Array(list) => list,
        other => return Err(wrong_type(op, index, "header array", &other)),
    };
    list.into_iter()
        .map(|pair| match pair {
            OwnedValue::Array(pair) => {
                let [name, value]: [OwnedValue; 2] = pair
                    .try_into()
                    .map_err(|_| wrong_type(op, index, "header pair", &OwnedValue::Null))?;
                Ok((string(op, index, name)?, string(op, index, value)?))
            }
            other => Err(wrong_type(op, index, "header pair", &other)),
        })
        .collect()
}

/// `Option<u64>` as serde_v8 gives it: a Number, or null.
fn optional_u64(value: Option<u64>) -> OwnedValue {
    match value {
        Some(value) => OwnedValue::U64(value),
        None => OwnedValue::Null,
    }
}

/// `Vec<(ByteString, ByteString)>`: the header list, as pairs of byte strings.
fn header_pairs(
    op: u32,
    index: usize,
    value: OwnedValue,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, ServiceError> {
    let list = match value {
        OwnedValue::Array(list) => list,
        other => return Err(wrong_type(op, index, "header array", &other)),
    };
    list.into_iter()
        .map(|pair| match pair {
            OwnedValue::Array(pair) => {
                let [name, value]: [OwnedValue; 2] = pair
                    .try_into()
                    .map_err(|_| wrong_type(op, index, "header pair", &OwnedValue::Null))?;
                Ok((header_bytes(op, index, name)?, header_bytes(op, index, value)?))
            }
            other => Err(wrong_type(op, index, "header pair", &other)),
        })
        .collect()
}

fn header_bytes(op: u32, index: usize, value: OwnedValue) -> Result<Vec<u8>, ServiceError> {
    match value {
        OwnedValue::Bytes(bytes) => Ok(bytes),
        OwnedValue::Str(text) => Ok(text.into_bytes()),
        other => Err(wrong_type(op, index, "byte string", &other)),
    }
}

#[cfg(test)]
#[path = "service_network_tests.rs"]
mod tests;
