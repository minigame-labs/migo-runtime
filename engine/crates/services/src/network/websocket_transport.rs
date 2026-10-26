//! Optional RFC7692 transport. The ordinary connection keeps its inline path;
//! negotiated compression uses the same fair IO executor as HTTP decoding.
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use migo_io::polling::Offloaded;
use migo_io::pools::{ByteTicket, IoPools};
use migo_io::task::PoolKind;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite::protocol::frame::{
    Frame,
    coding::{Data, OpCode},
};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

use super::websocket::{WS_MAX_FRAME_BYTES, WS_MAX_MESSAGE_BYTES, WsEvent};
use super::websocket_deflate::{self as deflate, Deflater, Inflater, Negotiated};
use crate::ServiceError;

const CODEC_WORKSPACE: u64 = 1024 * 1024;
const COMPRESS_AT: usize = 1024;

type Flight<T> = Offloaded<Pin<Box<dyn Future<Output = T> + Send>>>;
type ReceiveResult<S> = (ReceiveState<S>, Result<ReadStep, ServiceError>);
type SendResult<S> = (SendState<S>, Result<(), ServiceError>);

pub(super) enum ReadStep {
    Event(WsEvent),
    Control,
    Fragment,
}

pub(super) struct Receiver<S> {
    inner: Option<ReceiveState<S>>,
    pending: Option<Flight<ReceiveResult<S>>>,
    pools: IoPools,
    compressed: bool,
}

struct ReceiveState<S> {
    stream: SplitStream<WebSocketStream<S>>,
    parameters: Option<Negotiated>,
    decoder: Option<Inflater>,
    kind: Option<Data>,
    workspace: Option<ByteTicket>,
    message_credit: Option<ByteTicket>,
    pools: IoPools,
    _slot: Arc<tokio::sync::OwnedSemaphorePermit>,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> Receiver<S> {
    pub fn new(
        stream: SplitStream<WebSocketStream<S>>,
        parameters: Option<Negotiated>,
        pools: IoPools,
        slot: Arc<tokio::sync::OwnedSemaphorePermit>,
    ) -> Self {
        Self {
            inner: Some(ReceiveState {
                stream,
                parameters,
                decoder: None,
                kind: None,
                workspace: None,
                message_credit: None,
                pools: pools.clone(),
                _slot: slot,
            }),
            pending: None,
            pools,
            compressed: parameters.is_some(),
        }
    }

    pub async fn next(&mut self) -> Result<ReadStep, ServiceError> {
        if !self.compressed {
            let result = self.inner.as_mut().ok_or_else(closed)?.next().await;
            if result.is_err() {
                self.inner = None;
            }
            return result;
        }
        if self.pending.is_none() {
            // Framing stays Tungstenite's. Negotiating deflate must not add a
            // worker roundtrip to every small uncompressed game-state packet.
            // Only actual codec work or a large payload conversion is offloaded.
            let message = self.inner.as_mut().ok_or_else(closed)?.stream.next().await;
            let slow = match &message {
                Some(Ok(Message::Frame(_))) => true,
                Some(Ok(message)) => message.len() > deflate::CHUNK,
                _ => false,
            };
            if !slow {
                let inner = self.inner.as_mut().ok_or_else(closed)?;
                let event = inner.accept(message).await;
                inner.handoff(&event);
                if event.is_err() {
                    self.inner = None;
                }
                return event;
            }
            let mut inner = self.inner.take().ok_or_else(closed)?;
            let wire_bytes = message
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .map_or(0, |message| message.len() as u64);
            let wire_credit = self.pools.reserve_bytes(wire_bytes).map_err(error)?;
            let future: Pin<Box<dyn Future<Output = ReceiveResult<S>> + Send>> =
                Box::pin(async move {
                    let _wire_credit = wire_credit;
                    let result = inner.accept(message).await;
                    (inner, result)
                });
            // The codec reserves its workspace lazily and its output before
            // decompression. Own the admitted wire fragment before submitting.
            self.pending = Some(
                Offloaded::new(future, self.pools.clone(), PoolKind::Archive, 0).map_err(error)?,
            );
        }
        let result = self.pending.as_mut().unwrap().wait().await;
        self.pending = None;
        let (mut inner, event) = result.map_err(error)?;
        // A ready result can wait in the offloader after its caller stops
        // polling. Keep its credit with the tuple until this actual handoff.
        inner.handoff(&event);
        if event.is_ok() {
            self.inner = Some(inner);
        }
        event
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> ReceiveState<S> {
    fn handoff(&mut self, event: &Result<ReadStep, ServiceError>) {
        if matches!(
            event,
            Ok(ReadStep::Event(WsEvent::Text(_) | WsEvent::Binary(_)))
        ) {
            self.message_credit = None;
        }
    }

    async fn next(&mut self) -> Result<ReadStep, ServiceError> {
        let message = self.stream.next().await;
        self.accept(message).await
    }

    async fn accept(
        &mut self,
        message: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    ) -> Result<ReadStep, ServiceError> {
        Ok(match message {
            Some(Ok(Message::Text(text))) => {
                if self.parameters.is_some() {
                    self.message_credit =
                        Some(self.pools.reserve_bytes(text.len() as u64).map_err(error)?);
                }
                ReadStep::Event(WsEvent::Text(text.to_string()))
            }
            Some(Ok(Message::Binary(bytes))) => {
                if self.parameters.is_some() {
                    self.message_credit = Some(
                        self.pools
                            .reserve_bytes(bytes.len() as u64)
                            .map_err(error)?,
                    );
                }
                ReadStep::Event(WsEvent::Binary(Vec::from(bytes)))
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => ReadStep::Control,
            Some(Ok(Message::Frame(frame))) => {
                let parameters = self
                    .parameters
                    .ok_or_else(|| error("server sent an unnegotiated compressed message"))?;
                if self.decoder.is_none() {
                    self.workspace =
                        Some(self.pools.reserve_bytes(CODEC_WORKSPACE).map_err(error)?);
                    self.decoder = Some(Inflater::new(
                        parameters.server_no_context_takeover,
                        WS_MAX_MESSAGE_BYTES,
                    ));
                }
                if frame.header().rsv1 {
                    self.kind = match frame.header().opcode {
                        OpCode::Data(kind @ (Data::Text | Data::Binary)) if self.kind.is_none() => {
                            Some(kind)
                        }
                        _ => return Err(error("invalid compressed message start")),
                    };
                    self.message_credit = Some(
                        self.pools
                            .reserve_bytes((WS_MAX_MESSAGE_BYTES + WS_MAX_FRAME_BYTES) as u64)
                            .map_err(error)?,
                    );
                } else if frame.header().opcode != OpCode::Data(Data::Continue)
                    || self.kind.is_none()
                {
                    return Err(error("invalid compressed continuation"));
                }
                let decoded = self
                    .decoder
                    .as_mut()
                    .unwrap()
                    .fragment_job(frame.payload(), frame.header().is_final)
                    .map_err(error)?
                    .await
                    .map_err(error)?;
                match decoded {
                    None => ReadStep::Fragment,
                    Some(bytes) => {
                        let kind = self
                            .kind
                            .take()
                            .ok_or_else(|| error("compressed message has no type"))?;
                        ReadStep::Event(match kind {
                            Data::Text => {
                                WsEvent::Text(String::from_utf8(bytes).map_err(|_| {
                                    error("invalid UTF-8 in compressed text message")
                                })?)
                            }
                            Data::Binary => WsEvent::Binary(bytes),
                            _ => unreachable!(),
                        })
                    }
                }
            }
            Some(Ok(Message::Close(frame))) => {
                let (code, reason) = frame
                    .map(|frame| (frame.code.into(), frame.reason.to_string()))
                    .unwrap_or((1005, String::new()));
                ReadStep::Event(WsEvent::Close { code, reason })
            }
            Some(Err(fault)) => return Err(error(fault)),
            None => ReadStep::Event(WsEvent::Close {
                code: 1006,
                reason: String::new(),
            }),
        })
    }
}

pub(super) struct Sender<S> {
    inner: Option<SendState<S>>,
    pending: Option<Flight<SendResult<S>>>,
    pools: IoPools,
    compressed: bool,
}

struct SendState<S> {
    stream: SplitSink<WebSocketStream<S>, Message>,
    parameters: Option<Negotiated>,
    encoder: Option<Deflater>,
    workspace: Option<ByteTicket>,
    pools: IoPools,
    _slot: Arc<tokio::sync::OwnedSemaphorePermit>,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> Sender<S> {
    pub fn new(
        stream: SplitSink<WebSocketStream<S>, Message>,
        parameters: Option<Negotiated>,
        pools: IoPools,
        slot: Arc<tokio::sync::OwnedSemaphorePermit>,
    ) -> Self {
        Self {
            inner: Some(SendState {
                stream,
                parameters,
                encoder: None,
                workspace: None,
                pools: pools.clone(),
                _slot: slot,
            }),
            pending: None,
            pools,
            compressed: parameters.is_some(),
        }
    }

    async fn finish_pending(&mut self) -> Result<(), ServiceError> {
        if let Some(pending) = &mut self.pending {
            let result = pending.wait().await;
            self.pending = None;
            let (inner, sent) = result.map_err(error)?;
            if sent.is_ok() {
                self.inner = Some(inner);
            }
            sent?;
        }
        Ok(())
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_none() && self.pending.is_none()
    }

    #[cfg(test)]
    pub async fn send(
        &mut self,
        message: Message,
        deadline: tokio::time::Instant,
    ) -> Result<(), ServiceError> {
        let payload = self
            .pools
            .reserve_bytes(message.len() as u64)
            .map_err(error)?;
        self.send_admitted(message, deadline, payload).await
    }

    pub async fn send_admitted(
        &mut self,
        message: Message,
        deadline: tokio::time::Instant,
        payload: ByteTicket,
    ) -> Result<(), ServiceError> {
        if matches!(&message, Message::Text(_) | Message::Binary(_))
            && message.len() > WS_MAX_MESSAGE_BYTES
        {
            return Err(error("WebSocket message exceeds 8 MiB"));
        }
        // A previous fragmented plain send also owns its stream until it is
        // finished. Drain it before selecting the next message's fast path.
        self.finish_pending().await?;
        let encode = self.compressed
            && matches!(&message, Message::Text(_) | Message::Binary(_))
            && message.len() >= COMPRESS_AT;
        if !encode && message.len() <= deflate::CHUNK {
            let result = self
                .inner
                .as_mut()
                .ok_or_else(closed)?
                .stream
                .send(message)
                .await
                .map_err(error);
            if result.is_err() {
                self.inner = None;
            }
            return result;
        }
        // An abandoned wait still owns its exact previous message and codec
        // result. Complete it before changing the outgoing dictionary again.
        let retained = if encode {
            deflate::encoded_limit(message.len())
        } else {
            super::websocket::WS_WRITE_BUFFER_BYTES + deflate::CHUNK
        } as u64;
        let credit = self.pools.reserve_bytes(retained).map_err(error)?;
        let mut inner = self.inner.take().ok_or_else(closed)?;
        let future: Pin<Box<dyn Future<Output = SendResult<S>> + Send>> = Box::pin(async move {
            let _credit = credit;
            let _payload = payload;
            let result = tokio::time::timeout_at(deadline, inner.send(message))
                .await
                .map_err(|_| error("WebSocket write timeout"))
                .and_then(|result| result);
            (inner, result)
        });
        self.pending =
            Some(Offloaded::new(future, self.pools.clone(), PoolKind::Archive, 0).map_err(error)?);
        self.finish_pending().await
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> SendState<S> {
    async fn send(&mut self, message: Message) -> Result<(), ServiceError> {
        if !matches!(&message, Message::Text(_) | Message::Binary(_)) || message.len() < COMPRESS_AT
        {
            return self.stream.send(message).await.map_err(error);
        }
        let opcode = if message.is_text() {
            Data::Text
        } else {
            Data::Binary
        };
        let data = message.into_data();
        let Some(parameters) = self.parameters else {
            return self.frames(data, opcode, false).await;
        };
        if self.encoder.is_none() {
            self.workspace = Some(self.pools.reserve_bytes(CODEC_WORKSPACE).map_err(error)?);
            self.encoder = Some(Deflater::new(parameters.client_no_context_takeover));
        }
        let encoded = self
            .encoder
            .as_mut()
            .unwrap()
            .message_job(&data, data.len())
            .map_err(error)?
            .await
            .map_err(error)?;
        self.frames(bytes::Bytes::from(encoded), opcode, true).await
    }

    async fn frames(
        &mut self,
        data: bytes::Bytes,
        opcode: Data,
        compressed: bool,
    ) -> Result<(), ServiceError> {
        for start in (0..data.len().max(1)).step_by(deflate::CHUNK) {
            let end = (start + deflate::CHUNK).min(data.len());
            let mut frame = Frame::message(
                data.slice(start..end),
                OpCode::Data(if start == 0 { opcode } else { Data::Continue }),
                end == data.len(),
            );
            frame.header_mut().rsv1 = compressed && start == 0;
            self.stream
                .feed(Message::Frame(frame))
                .await
                .map_err(error)?;
            tokio::task::yield_now().await;
        }
        self.stream.flush().await.map_err(error)
    }
}

fn error(message: impl std::fmt::Display) -> ServiceError {
    ServiceError::generic(format!("WebSocket: {message}"))
}
fn closed() -> ServiceError {
    error("connection is closed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_io::pools::ByteAdmitError;
    use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

    fn available(pools: &IoPools) -> u64 {
        match pools.reserve_bytes(u64::MAX) {
            Err(ByteAdmitError::BytesExhausted { available, .. }) => available,
            _ => panic!("a fresh IO pool must report its finite byte budget"),
        }
    }

    #[test]
    fn a_receive_result_keeps_its_byte_credit_until_the_waiter_takes_it() {
        let mut failures = Vec::new();
        for mode in ["false", "true"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env("MIGO_WS_RECEIVE_BUDGET_CASE", mode)
                .args([
                    "--ignored",
                    "--exact",
                    "network::websocket_transport::tests::receive_result_budget_probe",
                    "--nocapture",
                ])
                .output()
                .expect("fresh-process receive budget probe");
            if !output.status.success() {
                failures.push(format!(
                    "mode={mode}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[tokio::test]
    #[ignore = "run by the parent in a fresh process, isolated from other IO-budget tests"]
    async fn receive_result_budget_probe() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let compressed = std::env::var("MIGO_WS_RECEIVE_BUDGET_CASE").expect("parent selects the isolated budget case").parse::<bool>().unwrap();
            {
                let pools = IoPools::new(7920);
                let baseline = available(&pools);
                let parameters = Some(Negotiated { server_no_context_takeover: false, client_no_context_takeover: false, server_max_window_bits: 15 });
                let (client, peer) = tokio::io::duplex(16 * 1024);
                let config = WebSocketConfig::default().external_deflate(true);
                let client = WebSocketStream::from_raw_socket(client, Role::Client, Some(config)).await;
                let mut peer = WebSocketStream::from_raw_socket(peer, Role::Server, Some(config)).await;
                let (_tx, rx) = client.split();
                let resources = super::super::resources::ResourceTable::new();
                let mut receiver = Receiver::new(rx, parameters, pools.clone(), Arc::new(resources.web_socket_slot().unwrap()));
                let mut state = receiver.inner.take().unwrap();
                let bytes = vec![42; 64 * 1024];
                let message = if compressed {
                    let encoded = Deflater::new(false).message(&bytes, bytes.len()).unwrap();
                    let mut frame = Frame::message(encoded, OpCode::Data(Data::Binary), true);
                    frame.header_mut().rsv1 = true;
                    Message::Frame(frame)
                } else { Message::Binary(bytes.clone().into()) };
                let (result, sent) = tokio::join!(state.next(), peer.send(message));
                sent.unwrap();
                let result = result.map(|event| match event {
                    ReadStep::Event(WsEvent::Binary(data)) => { assert_eq!(data, bytes); ReadStep::Event(WsEvent::Binary(data)) },
                    _ => panic!("fixture did not produce a complete binary message"),
                });
                let held = baseline - available(&pools);
                assert!(held >= if compressed { WS_MAX_MESSAGE_BYTES as u64 } else { bytes.len() as u64 },
                    "a queued ready result lost its output credit: compressed={compressed}, held={held}");
                // The native poll has produced this tuple; the caller has not
                // taken it. Exercise the real retained-result handoff next.
                let future: Pin<Box<dyn Future<Output = ReceiveResult<tokio::io::DuplexStream>> + Send>> = Box::pin(async move { (state, result) });
                receiver.pending = Some(Offloaded::new(future, pools.clone(), PoolKind::Archive, 0).unwrap());
                let delivered = receiver.next().await.unwrap();
                assert!(matches!(delivered, ReadStep::Event(WsEvent::Binary(_))));
                assert_eq!(baseline - available(&pools), if compressed { CODEC_WORKSPACE } else { 0 });
                drop(receiver);
                assert_eq!(available(&pools), baseline, "dropping the codec releases its workspace");
            }
        }).await.expect("budget probe must be bounded");
    }
}
