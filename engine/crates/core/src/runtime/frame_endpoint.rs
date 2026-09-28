//! The frame endpoint: the loopback WebSocket a Performance+ producer connects to.
//!
//! The producer is content JavaScript in WebKit's WebContent process, and a
//! WebSocket is the one channel it has that the host can push on -- a scheme
//! handler answers requests and cannot, and a frame-clock tick is something the
//! host starts. So every frame crosses this socket twice: the packet (with its
//! request for the next tick) up, and the tick (with the packet's verdict) down.
//!
//! ## Why the engine terminates it
//!
//! The host used to, with Network.framework, handing each message across the C
//! boundary and draining the downlink from a GCD queue the engine's waker
//! scheduled. Measured on devices with a game running, Network.framework alone
//! was about a quarter of the App process's CPU samples (iPhone 15 Pro: 23%,
//! ~3.4% of a core; iPhone XS Max: 27%), and a tick crossed three queues
//! between the frame clock and the socket: the waker's block, the channel's
//! pump, the connection's own queue. Here an uplink message is one `read(2)`
//! and a submit on the thread that read it, and a downlink message is one
//! unpark and one `writev(2)`.
//!
//! ## Threads
//!
//! Two, user-interactive on Apple like the render thread, because a tick is the
//! start of a frame and a packet is the end of one:
//!
//! - `migo-frame-io` accepts, performs the handshake, and reads. A packet is
//!   validated and credited on it, which is what the session's submit path is
//!   built for: no hop between the socket and the ingress.
//! - `migo-frame-downlink` sleeps until the session's downlink waker unparks
//!   it, then drains what the session owes the producer and writes it.
//!
//! One thread would need to wait for a readable socket and a wake-up at once,
//! and the standard library has no portable way to; two blocking threads cost
//! no CPU while they wait.
//!
//! ## One producer at a time
//!
//! The io thread serves one connection and accepts the next when it ends. A
//! producer that connects while another is being served waits in the listen
//! backlog and never reaches the ingress, so two producers' sequences cannot
//! interleave -- the failure that would look like corruption. And a producer
//! rebuilt after a WebContent termination, which can connect before the old
//! socket's end has been read, is served as soon as it has been, not refused.
//!
//! ## What it does not do
//!
//! It does not know what a frame is. Messages go to the door the wire names
//! (`is_control_message`, `is_service_message`, otherwise a frame packet), and
//! the bytes sent back are the session's own encoding: a transport that built
//! records would be another implementation of a wire format that already has
//! two.

use std::io::{self, IoSlice, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::{self, JoinHandle, Thread};
use std::time::Duration;

use migo_services::network::tungstenite::{
    self,
    protocol::{
        CloseFrame, Message, WebSocketConfig,
        frame::{
            FrameHeader,
            coding::{CloseCode, Data, OpCode},
        },
    },
};
use parking_lot::Mutex;
use tracing::{debug, error, warn};

use frame_wire::{IngressDecision, IngressOutcome, control::ControlError};

use super::external::{ControlOutcome, DownlinkWaker};
use super::external_services::{ServiceAdmission, ServiceSubmitError};

/// What a session's transports have done, counted where the work happens
/// rather than by whichever transport carried it.
///
/// A frame reaches the ingress over the socket or, above the socket ceiling,
/// through the content origin; a service message likewise. Both arrive at the
/// same submit, and that is where they are counted, so the numbers do not
/// depend on a packet's size or on which host code carried it.
#[derive(Debug, Default)]
pub(crate) struct TransportCounters {
    frames_received: AtomicU64,
    frames_accepted: AtomicU64,
    frames_deferred: AtomicU64,
    frames_refused: AtomicU64,
    control_messages_received: AtomicU64,
    control_messages_refused: AtomicU64,
    last_control_refusal_code: AtomicU32,
    service_messages_received: AtomicU64,
    service_messages_refused: AtomicU64,
    last_service_refusal_code: AtomicU32,
    downlink_messages_sent: AtomicU64,
    service_messages_sent: AtomicU64,
    downlink_wakes: AtomicU64,
    downlink_records_dropped: AtomicU64,
    sends_without_producer: AtomicU64,
    producers_connected: AtomicU64,
    producer_connected: AtomicBool,
}

/// A snapshot of [`TransportCounters`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FrameTransportStatistics {
    /// Packets offered to the ingress, by either uplink.
    pub frames_received: u64,
    pub frames_accepted: u64,
    /// Held until the packet before them arrived: the two uplinks reorder.
    pub frames_deferred: u64,
    /// Refused for any reason, including a full credit window.
    pub frames_refused: u64,
    /// The producer's requests for a frame.
    pub control_messages_received: u64,
    pub control_messages_refused: u64,
    /// The most recent control refusal's code; 0 when there has been none.
    pub last_control_refusal_code: u32,
    pub service_messages_received: u64,
    pub service_messages_refused: u64,
    /// The most recent service refusal's code; 0 when there has been none.
    pub last_service_refusal_code: u32,
    /// Downlink envelopes (verdicts and ticks) written to the producer.
    pub downlink_messages_sent: u64,
    /// Service stream messages (answers and events) written to the producer.
    pub service_messages_sent: u64,
    /// Drains the session asked for through its downlink waker.
    pub downlink_wakes: u64,
    /// Downlink records the session dropped because nothing drained them in
    /// time. Every record is absolute, so the next one corrects the producer.
    pub downlink_records_dropped: u64,
    /// Messages drained while no producer could take them: between a
    /// WebContent termination and the rebuilt producer's connection this is
    /// the expected state, not a fault.
    pub sends_without_producer: u64,
    /// Producers that completed the handshake.
    pub producers_connected: u64,
    /// Whether one is connected now.
    pub producer_connected: bool,
}

impl TransportCounters {
    pub(crate) fn frame(&self, outcome: &IngressOutcome) {
        self.frames_received.fetch_add(1, Ordering::Relaxed);
        let bucket = match outcome.decision {
            IngressDecision::Accepted => &self.frames_accepted,
            IngressDecision::Deferred => &self.frames_deferred,
            IngressDecision::WouldBlock
            | IngressDecision::Rejected
            | IngressDecision::GenerationLost => &self.frames_refused,
        };
        bucket.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn control(&self, result: &Result<ControlOutcome, ControlError>) {
        self.control_messages_received
            .fetch_add(1, Ordering::Relaxed);
        if let Err(error) = result {
            self.control_messages_refused
                .fetch_add(1, Ordering::Relaxed);
            self.last_control_refusal_code
                .store(error.code(), Ordering::Relaxed);
        }
    }

    pub(crate) fn service(&self, result: &Result<ServiceAdmission, ServiceSubmitError>) {
        self.service_messages_received
            .fetch_add(1, Ordering::Relaxed);
        if let Err(ServiceSubmitError::Refused(error)) = result {
            self.service_messages_refused
                .fetch_add(1, Ordering::Relaxed);
            self.last_service_refusal_code
                .store(error.code(), Ordering::Relaxed);
        }
    }

    pub(crate) fn snapshot(&self) -> FrameTransportStatistics {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        FrameTransportStatistics {
            frames_received: read(&self.frames_received),
            frames_accepted: read(&self.frames_accepted),
            frames_deferred: read(&self.frames_deferred),
            frames_refused: read(&self.frames_refused),
            control_messages_received: read(&self.control_messages_received),
            control_messages_refused: read(&self.control_messages_refused),
            last_control_refusal_code: self.last_control_refusal_code.load(Ordering::Relaxed),
            service_messages_received: read(&self.service_messages_received),
            service_messages_refused: read(&self.service_messages_refused),
            last_service_refusal_code: self.last_service_refusal_code.load(Ordering::Relaxed),
            downlink_messages_sent: read(&self.downlink_messages_sent),
            service_messages_sent: read(&self.service_messages_sent),
            downlink_wakes: read(&self.downlink_wakes),
            downlink_records_dropped: read(&self.downlink_records_dropped),
            sends_without_producer: read(&self.sends_without_producer),
            producers_connected: read(&self.producers_connected),
            producer_connected: self.producer_connected.load(Ordering::Relaxed),
        }
    }
}

/// Why the endpoint did not start.
#[derive(Debug)]
pub enum FrameEndpointError {
    /// The session already has one. A stopped endpoint is not restarted: its
    /// threads are joined with the session, and a second would compete with
    /// them for nothing.
    AlreadyStarted,
    /// The loopback port could not be bound, or a thread not started.
    Io(io::Error),
}

impl std::fmt::Display for FrameEndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyStarted => f.write_str("the session's frame endpoint was already started"),
            Self::Io(error) => write!(f, "the frame endpoint could not start: {error}"),
        }
    }
}

impl std::error::Error for FrameEndpointError {}

/// The session the endpoint serves: its three doors and its two drains.
///
/// [`super::external::ExternalFrameSession`] hands out its own, detached from
/// the thread handle; the tests implement it without a renderer. The doors
/// count what they are given (see [`TransportCounters`]), so the endpoint does
/// not.
pub(crate) trait EndpointTarget: Send + Sync + 'static {
    fn submit_frame(&self, packet: &[u8]);
    fn submit_control(&self, message: &[u8]);
    /// May block while the session's work queue is full: back-pressure on the
    /// socket, as it was on the queue the host used to read it on.
    fn submit_service(&self, message: &[u8]);
    fn take_service_message(&self) -> Option<Vec<u8>>;
    /// See `ExternalFrameSession::take_downlink`.
    fn take_downlink(&self, out: &mut [u8]) -> usize;
    fn take_downlink_drops(&self) -> u32;
    fn set_downlink_waker(&self, waker: Option<DownlinkWaker>);
    fn counters(&self) -> &TransportCounters;
}

/// The largest message the endpoint reads: the largest any door accepts. A
/// frame packet is bounded well below this (`frame_wire::MAX_TOTAL_BYTES`), a
/// service message is bounded by exactly this, and a control message is a few
/// words. Anything larger is refused by the WebSocket layer before it is
/// buffered.
const MAX_UPLINK_MESSAGE_BYTES: usize = {
    let frame = frame_wire::MAX_TOTAL_BYTES as usize;
    let service = frame_wire::service::MAX_SERVICE_MESSAGE_BYTES;
    if frame > service { frame } else { service }
};

/// The downlink buffer. The queue holds at most its capacity of records of at
/// most eight words, so 4 KiB cannot be exceeded; a message that did not fit
/// would be sent as two, because the drain takes whole records.
const DOWNLINK_BUFFER_BYTES: usize = 4096;

/// A server frame header: two bytes, and eight more for the longest length. No
/// mask -- a server does not mask (RFC 6455 section 5.1).
const MAX_SERVER_HEADER_BYTES: usize = 10;

/// How long a connection has to complete its handshake. WebKit sends its
/// request at once; a connection that sends nothing would otherwise hold the
/// io thread, and with it the endpoint, forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the io thread waits before accepting again after `accept` failed
/// for a reason that retrying at once would repeat (out of descriptors).
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// A running endpoint. Stopping is asynchronous (see [`Self::stop`]); dropping
/// stops it and joins its threads.
pub(crate) struct FrameEndpoint {
    port: u16,
    state: Arc<EndpointState>,
    io: Option<JoinHandle<()>>,
    downlink: Option<JoinHandle<()>>,
}

struct EndpointState {
    target: Arc<dyn EndpointTarget>,
    /// The listener's own address, which `stop` connects to so a blocked
    /// `accept` returns: the standard library has no portable way to interrupt
    /// one.
    address: SocketAddr,
    stopping: AtomicBool,
    /// The connection being served, from `accept` until it ends. Its lock is
    /// also the one `stop` sets `stopping` under, so a connection is either
    /// published before `stop` looks (and shut down by it) or sees `stopping`
    /// and is dropped: there is no window in which it is neither.
    connection: Mutex<Option<Arc<Connection>>>,
    /// A drain has been asked for and not yet started. Coalesces wake-ups that
    /// arrive faster than the drain runs, so a busy frame clock costs one
    /// unpark, not many.
    drain_pending: AtomicBool,
}

impl EndpointState {
    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    /// Ask the downlink thread to drain. `park`/`unpark` carry a token, so an
    /// unpark that lands before the thread parks is not lost.
    fn request_drain(&self, downlink: &Thread) -> bool {
        let first = !self.drain_pending.swap(true, Ordering::AcqRel);
        if first {
            downlink.unpark();
        }
        first
    }

    /// One message from the producer, to the door the wire names.
    fn deliver(&self, message: &[u8]) {
        if frame_wire::control::is_control_message(message) {
            self.target.submit_control(message);
        } else if frame_wire::service::is_service_message(message) {
            self.target.submit_service(message);
        } else {
            self.target.submit_frame(message);
        }
    }

    /// Everything the session owes the producer, in the order it has always
    /// been sent: the service stream first -- its answers and the host's input
    /// events -- then the frame records. Input queued before a tick has to
    /// reach content before that tick's frame callbacks run, or the frame is
    /// drawn against input one frame old; the producer handles messages in the
    /// order they arrive, and one thread writing them keeps that order.
    fn drain(&self, records: &mut [u8]) {
        let connection = self.connection.lock().clone();
        let counters = self.target.counters();
        while let Some(message) = self.target.take_service_message() {
            if self.send(connection.as_deref(), &message) {
                counters
                    .service_messages_sent
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        loop {
            let written = self.target.take_downlink(records);
            if written == 0 {
                break;
            }
            if self.send(connection.as_deref(), &records[..written]) {
                counters
                    .downlink_messages_sent
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        let dropped = self.target.take_downlink_drops();
        if dropped > 0 {
            counters
                .downlink_records_dropped
                .fetch_add(u64::from(dropped), Ordering::Relaxed);
        }
    }

    /// Whether the message was written. One that could not be is dropped and
    /// counted, as the host's transport always did: a message owed to a
    /// producer that has gone is owed to nobody -- the rebuilt one starts from
    /// what is queued when it connects, and every downlink record is absolute.
    fn send(&self, connection: Option<&Connection>, payload: &[u8]) -> bool {
        let written = match connection {
            None => false,
            Some(connection) => match connection.send_binary(payload) {
                Ok(written) => written,
                Err(error) => {
                    // The producer went away mid-write. Shut the socket so the
                    // io thread's read ends now rather than on its own error.
                    debug!("frame endpoint: a downlink write failed: {error}");
                    connection.shutdown();
                    false
                }
            },
        };
        if !written {
            self.target
                .counters()
                .sends_without_producer
                .fetch_add(1, Ordering::Relaxed);
        }
        written
    }
}

/// The producer's socket, written by two threads: the downlink thread's data
/// frames and the io thread's handshake response and control replies. Each
/// write is whole frames under one lock, so they interleave only between
/// frames.
struct Connection {
    /// For `shutdown`, which takes no lock: a write blocked on a producer that
    /// stopped reading holds the writer's.
    socket: TcpStream,
    writer: Mutex<Writer>,
}

struct Writer {
    stream: TcpStream,
    /// Whether data frames may be written: from the end of the handshake until
    /// a Close is received or sent. Nothing may follow a Close (RFC 6455
    /// section 5.5.1), and the reply to one is written on the io thread's next
    /// read -- after this is cleared, so no downlink message slips in behind it.
    open: bool,
}

impl Connection {
    fn new(socket: &TcpStream) -> io::Result<Self> {
        Ok(Self {
            socket: socket.try_clone()?,
            writer: Mutex::new(Writer {
                stream: socket.try_clone()?,
                open: false,
            }),
        })
    }

    /// One binary message as one frame, header and payload in one `writev`.
    /// `Ok(false)` when the connection is not open for data.
    fn send_binary(&self, payload: &[u8]) -> io::Result<bool> {
        let header = FrameHeader {
            is_final: true,
            opcode: OpCode::Data(Data::Binary),
            ..FrameHeader::default()
        };
        let length = payload.len() as u64;
        let mut bytes = [0u8; MAX_SERVER_HEADER_BYTES];
        let header_length = header.len(length);
        header
            .format(length, &mut &mut bytes[..])
            .expect("a server frame header fits in ten bytes");

        let mut writer = self.writer.lock();
        if !writer.open {
            return Ok(false);
        }
        write_all_vectored(
            &mut writer.stream,
            &mut [IoSlice::new(&bytes[..header_length]), IoSlice::new(payload)],
        )?;
        Ok(true)
    }

    fn write_control(&self, bytes: &[u8]) -> io::Result<()> {
        self.writer.lock().stream.write_all(bytes)
    }

    fn set_open(&self, open: bool) {
        self.writer.lock().open = open;
    }

    fn shutdown(&self) {
        // Already shut down, or already reset by the peer: either way there is
        // nothing left to stop.
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}

/// `write_all` for two slices, without joining them into one buffer first.
fn write_all_vectored(stream: &mut TcpStream, mut slices: &mut [IoSlice<'_>]) -> io::Result<()> {
    while !slices.is_empty() {
        match stream.write_vectored(slices) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => IoSlice::advance_slices(&mut slices, written),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// The io thread's end of the socket, as tungstenite sees it: reads from the
/// socket, and writes -- the handshake response, pongs, the reply to a Close --
/// through the connection's writer, so they cannot land inside a downlink
/// frame.
struct ProducerStream {
    read: TcpStream,
    connection: Arc<Connection>,
}

impl Read for ProducerStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read.read(buf)
    }
}

impl Write for ProducerStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // All of it, under one lock: tungstenite hands over whole frames, and
        // accepting fewer bytes would let it split one across two calls.
        self.connection.write_control(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl FrameEndpoint {
    /// Bind a loopback port, start both threads, and take over the session's
    /// downlink waker.
    pub(crate) fn start(target: Arc<dyn EndpointTarget>) -> io::Result<Self> {
        // Loopback only: an endpoint that listened on every interface would
        // accept a producer from another machine. The literal address, not a
        // name: `localhost` does not resolve inside WKWebView, and 127.0.0.0/8
        // is potentially trustworthy, so the page keeps a secure context.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let state = Arc::new(EndpointState {
            target,
            address,
            stopping: AtomicBool::new(false),
            connection: Mutex::new(None),
            drain_pending: AtomicBool::new(false),
        });

        let downlink = thread::Builder::new()
            .name("migo-frame-downlink".into())
            .spawn({
                let state = Arc::clone(&state);
                move || run_downlink(&state)
            })?;
        let downlink_thread = downlink.thread().clone();
        let io = thread::Builder::new().name("migo-frame-io".into()).spawn({
            let state = Arc::clone(&state);
            let downlink_thread = downlink_thread.clone();
            move || run_io(&state, listener, &downlink_thread)
        });
        let io = match io {
            Ok(io) => io,
            Err(error) => {
                state.stopping.store(true, Ordering::Release);
                downlink_thread.unpark();
                if downlink.join().is_err() {
                    error!("frame endpoint: the downlink thread panicked");
                }
                return Err(error);
            }
        };

        // Last, once there is a thread to wake: anything queued before now
        // waits in the queue, and the producer's connection drains it.
        let waker_state = Arc::clone(&state);
        state.target.set_downlink_waker(Some(Box::new(move || {
            if waker_state.request_drain(&downlink_thread) {
                waker_state
                    .target
                    .counters()
                    .downlink_wakes
                    .fetch_add(1, Ordering::Relaxed);
            }
        })));

        Ok(Self {
            port: address.port(),
            state,
            io: Some(io),
            downlink: Some(downlink),
        })
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// Whether the endpoint is serving, as opposed to stopped and waiting to be
    /// joined.
    pub(crate) fn is_running(&self) -> bool {
        !self.state.stopping()
    }

    /// Stop serving: clear the session's downlink waker, close the producer's
    /// socket and stop accepting. Idempotent, and it does not wait for the
    /// threads -- the io thread can be inside a service submit that waits for
    /// the session thread, which a caller of this may be holding up -- so they
    /// are joined when the endpoint is dropped. After this returns nothing more
    /// is written to the producer, and at most the one message already being
    /// delivered reaches the session.
    pub(crate) fn stop(&self) {
        let connection = {
            let mut slot = self.state.connection.lock();
            if self.state.stopping.swap(true, Ordering::AcqRel) {
                return;
            }
            slot.take()
        };
        // Clearing returns only once no call to the waker is in progress.
        self.state.target.set_downlink_waker(None);
        if let Some(connection) = connection {
            connection.shutdown();
        }
        // An io thread in `accept` returns with this connection, sees
        // `stopping`, and drops it. Refused means there is no listener left to
        // wake: the io thread saw the shutdown above and has already returned.
        match TcpStream::connect(self.state.address) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
            Err(error) => {
                warn!("frame endpoint: could not wake the io thread to stop it: {error}");
            }
        }
        for handle in [&self.io, &self.downlink].into_iter().flatten() {
            handle.thread().unpark();
        }
    }
}

impl Drop for FrameEndpoint {
    fn drop(&mut self) {
        self.stop();
        for (name, handle) in [("io", self.io.take()), ("downlink", self.downlink.take())] {
            if let Some(handle) = handle
                && handle.join().is_err()
            {
                error!("frame endpoint: the {name} thread panicked");
            }
        }
    }
}

fn run_downlink(state: &EndpointState) {
    shared::thread_priority::set_current_thread_priority(
        shared::thread_priority::Priority::Display,
    );
    let mut records = vec![0u8; DOWNLINK_BUFFER_BYTES];
    loop {
        if state.stopping() {
            return;
        }
        // Cleared before the drain, so a record queued while it runs asks for
        // another rather than waiting for the next one.
        if !state.drain_pending.swap(false, Ordering::AcqRel) {
            thread::park();
            continue;
        }
        state.drain(&mut records);
    }
}

fn run_io(state: &EndpointState, listener: TcpListener, downlink: &Thread) {
    shared::thread_priority::set_current_thread_priority(
        shared::thread_priority::Priority::Display,
    );
    loop {
        let socket = match listener.accept() {
            Ok((socket, _)) => socket,
            Err(_) if state.stopping() => return,
            // The peer gave up before it was accepted, or a signal landed:
            // nothing to wait out.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionAborted | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) => {
                // Out of descriptors, most likely: retrying at once would spin.
                warn!("frame endpoint: accept failed: {error}");
                thread::park_timeout(ACCEPT_BACKOFF);
                continue;
            }
        };
        if state.stopping() {
            return;
        }
        serve(state, socket, downlink);
        if state.stopping() {
            return;
        }
    }
}

/// One producer, from its handshake to its end.
fn serve(state: &EndpointState, socket: TcpStream, downlink: &Thread) {
    // A tick is a few dozen bytes. With Nagle's algorithm a small write waits
    // for the previous one's acknowledgement, and the peer's delayed ACK can
    // hold that for tens of milliseconds -- a frame late, on the path this
    // endpoint exists to shorten.
    if let Err(error) = socket.set_nodelay(true) {
        warn!("frame endpoint: could not disable Nagle's algorithm: {error}");
    }
    if let Err(error) = shared::socket::suppress_sigpipe(&socket) {
        // Serving it anyway would let a producer that dies mid-write end the
        // app: refuse it instead, and the producer reports a failed channel.
        warn!("frame endpoint: could not make the producer's socket SIGPIPE-safe: {error}");
        return;
    }
    let connection = match Connection::new(&socket) {
        Ok(connection) => Arc::new(connection),
        Err(error) => {
            warn!("frame endpoint: could not take the producer's socket: {error}");
            return;
        }
    };
    {
        let mut slot = state.connection.lock();
        if state.stopping() {
            return;
        }
        *slot = Some(Arc::clone(&connection));
    }

    let counters = state.target.counters();
    let connected = run_connection(state, socket, &connection, downlink);

    connection.set_open(false);
    connection.shutdown();
    {
        let mut slot = state.connection.lock();
        if slot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &connection))
        {
            *slot = None;
        }
    }
    if connected {
        counters.producer_connected.store(false, Ordering::Relaxed);
    }
}

/// Whether the producer completed its handshake.
fn run_connection(
    state: &EndpointState,
    socket: TcpStream,
    connection: &Arc<Connection>,
    downlink: &Thread,
) -> bool {
    if let Err(error) = socket.set_read_timeout(Some(HANDSHAKE_TIMEOUT)) {
        warn!("frame endpoint: could not bound the handshake: {error}");
        return false;
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_UPLINK_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_UPLINK_MESSAGE_BYTES));
    let stream = ProducerStream {
        read: socket,
        connection: Arc::clone(connection),
    };
    let mut producer = match tungstenite::accept_with_config(stream, Some(config)) {
        Ok(producer) => producer,
        Err(error) => {
            if !state.stopping() {
                warn!("frame endpoint: a connection failed its handshake: {error}");
            }
            return false;
        }
    };
    if let Err(error) = producer.get_ref().read.set_read_timeout(None) {
        warn!("frame endpoint: could not clear the handshake's timeout: {error}");
        return true;
    }

    connection.set_open(true);
    let counters = state.target.counters();
    counters.producers_connected.fetch_add(1, Ordering::Relaxed);
    counters.producer_connected.store(true, Ordering::Relaxed);
    // A producer that has just connected has missed whatever was queued while
    // it was away. Draining now rather than at the next tick means it learns
    // its credit level at once, which it needs before it can send anything.
    state.request_drain(downlink);

    loop {
        let message = producer.read();
        if state.stopping() {
            return true;
        }
        match message {
            Ok(Message::Binary(message)) => state.deliver(&message),
            // The reply is queued and written by the next read, which then
            // ends the connection: closed for data first.
            Ok(Message::Close(_)) => connection.set_open(false),
            Ok(Message::Text(_)) => {
                // The producer speaks binary only; a text message is something
                // else connected to this port, and nothing it says is read.
                warn!("frame endpoint: closing a connection that sent text");
                connection.set_open(false);
                let reason = CloseFrame {
                    code: CloseCode::Unsupported,
                    reason: "the frame endpoint reads binary messages only".into(),
                };
                if producer.close(Some(reason)).is_err() {
                    return true;
                }
            }
            // Answered by tungstenite on the next read.
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return true;
            }
            // The producer's process went away without a closing handshake:
            // WebContent was terminated, or the page was torn down.
            Err(tungstenite::Error::Io(error)) => {
                debug!("frame endpoint: the producer's connection ended: {error}");
                return true;
            }
            Err(error) => {
                warn!("frame endpoint: closing the producer's connection: {error}");
                return true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::net::TcpStream;
    use std::sync::mpsc;
    use std::time::Instant;

    use migo_services::network::tungstenite::{self, Message, WebSocket, protocol::CloseFrame};

    use super::*;

    /// A session with no renderer: what arrives at each door is recorded, and
    /// what it owes the producer is whatever the test queued.
    #[derive(Default)]
    struct FakeSession {
        frames: Mutex<Vec<Vec<u8>>>,
        controls: Mutex<Vec<Vec<u8>>>,
        services: Mutex<Vec<Vec<u8>>>,
        service_out: Mutex<VecDeque<Vec<u8>>>,
        downlink_out: Mutex<VecDeque<Vec<u8>>>,
        drops: AtomicU32,
        waker: Mutex<Option<DownlinkWaker>>,
        counters: TransportCounters,
        delivered: Mutex<Option<mpsc::Sender<()>>>,
    }

    impl FakeSession {
        fn wake(&self) {
            if let Some(waker) = self.waker.lock().as_ref() {
                waker();
            }
        }

        fn delivered(&self) {
            if let Some(sender) = self.delivered.lock().as_ref() {
                let _ = sender.send(());
            }
        }
    }

    impl EndpointTarget for FakeSession {
        fn submit_frame(&self, packet: &[u8]) {
            self.frames.lock().push(packet.to_vec());
            self.delivered();
        }
        fn submit_control(&self, message: &[u8]) {
            self.controls.lock().push(message.to_vec());
            self.delivered();
        }
        fn submit_service(&self, message: &[u8]) {
            self.services.lock().push(message.to_vec());
            self.delivered();
        }
        fn take_service_message(&self) -> Option<Vec<u8>> {
            self.service_out.lock().pop_front()
        }
        fn take_downlink(&self, out: &mut [u8]) -> usize {
            let Some(next) = self.downlink_out.lock().pop_front() else {
                return 0;
            };
            out[..next.len()].copy_from_slice(&next);
            next.len()
        }
        fn take_downlink_drops(&self) -> u32 {
            self.drops.swap(0, Ordering::Relaxed)
        }
        fn set_downlink_waker(&self, waker: Option<DownlinkWaker>) {
            *self.waker.lock() = waker;
        }
        fn counters(&self) -> &TransportCounters {
            &self.counters
        }
    }

    const WAIT: Duration = Duration::from_secs(5);

    fn start() -> (Arc<FakeSession>, FrameEndpoint, mpsc::Receiver<()>) {
        let session = Arc::new(FakeSession::default());
        let (sender, receiver) = mpsc::channel();
        *session.delivered.lock() = Some(sender);
        let endpoint = FrameEndpoint::start(session.clone()).expect("the endpoint starts");
        (session, endpoint, receiver)
    }

    fn connect(endpoint: &FrameEndpoint) -> WebSocket<TcpStream> {
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, endpoint.port())).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        let url = format!("ws://127.0.0.1:{}/", endpoint.port());
        let (socket, _) = tungstenite::client(url, stream).expect("the handshake completes");
        socket
    }

    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting until {what}");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn binary(socket: &mut WebSocket<TcpStream>) -> Vec<u8> {
        match socket.read().expect("a message arrives") {
            Message::Binary(bytes) => bytes.to_vec(),
            other => panic!("expected a binary message, got {other:?}"),
        }
    }

    fn control_message() -> Vec<u8> {
        frame_wire::control::encode_control(&[frame_wire::control::ControlRecord::RequestFrame {
            generation: 1,
        }])
    }

    fn service_message() -> Vec<u8> {
        let mut message = Vec::new();
        message.extend_from_slice(&frame_wire::service::MAGIC_SERVICE.to_le_bytes());
        message.extend_from_slice(&[0u8; 12]);
        assert!(frame_wire::service::is_service_message(&message));
        message
    }

    #[test]
    fn the_listener_is_loopback_only() {
        let (_session, endpoint, _) = start();
        assert_eq!(endpoint.state.address.ip(), Ipv4Addr::LOCALHOST);
        assert_ne!(endpoint.port(), 0);
    }

    #[test]
    fn each_message_reaches_the_door_the_wire_names_unchanged() {
        let (session, endpoint, delivered) = start();
        let mut producer = connect(&endpoint);

        // Large enough for a 64-bit length and several reads.
        let packet: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        producer.send(Message::binary(packet.clone())).unwrap();
        producer.send(Message::binary(control_message())).unwrap();
        producer.send(Message::binary(service_message())).unwrap();
        for _ in 0..3 {
            delivered.recv_timeout(WAIT).expect("delivered");
        }

        assert_eq!(*session.frames.lock(), vec![packet]);
        assert_eq!(*session.controls.lock(), vec![control_message()]);
        assert_eq!(*session.services.lock(), vec![service_message()]);
    }

    #[test]
    fn a_wake_sends_the_service_stream_first_then_the_frame_records() {
        let (session, endpoint, _) = start();
        let mut producer = connect(&endpoint);
        wait_until("the producer is connected", || {
            session.counters.snapshot().producer_connected
        });

        // Header lengths on both sides of each boundary: 7-bit, 16-bit, 64-bit.
        let service_a = vec![1u8; 125];
        let service_b = vec![2u8; 126];
        let records = vec![3u8; 4096];
        let large = vec![4u8; 70_000];
        session
            .service_out
            .lock()
            .extend([service_a.clone(), service_b.clone(), large.clone()]);
        session.downlink_out.lock().push_back(records.clone());
        session.wake();

        assert_eq!(binary(&mut producer), service_a);
        assert_eq!(binary(&mut producer), service_b);
        assert_eq!(binary(&mut producer), large);
        assert_eq!(binary(&mut producer), records);
        wait_until("the counts are in", || {
            let statistics = session.counters.snapshot();
            statistics.service_messages_sent == 3 && statistics.downlink_messages_sent == 1
        });
        assert!(session.counters.snapshot().downlink_wakes >= 1);
    }

    #[test]
    fn nothing_is_sent_without_a_wake_and_a_new_producer_is_told_what_waits() {
        let (session, endpoint, _) = start();
        session.downlink_out.lock().push_back(vec![9u8; 32]);

        // Queued before anyone connected, and no wake: the connection drains it.
        let mut producer = connect(&endpoint);
        assert_eq!(binary(&mut producer), vec![9u8; 32]);

        // Queued now, no wake: stays queued.
        session.downlink_out.lock().push_back(vec![8u8; 16]);
        producer
            .get_mut()
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        assert!(producer.read().is_err(), "nothing arrives without a wake");
        assert_eq!(session.downlink_out.lock().len(), 1);
        assert_eq!(session.counters.snapshot().downlink_wakes, 0);
    }

    #[test]
    fn a_drain_with_no_producer_is_counted_and_dropped() {
        let (session, _endpoint, _) = start();
        session.downlink_out.lock().push_back(vec![1u8; 8]);
        session.service_out.lock().push_back(vec![2u8; 8]);
        session.drops.store(3, Ordering::Relaxed);
        session.wake();
        wait_until("the drain ran", || {
            session.counters.snapshot().sends_without_producer == 2
        });
        let statistics = session.counters.snapshot();
        assert_eq!(statistics.downlink_records_dropped, 3);
        assert_eq!(statistics.downlink_messages_sent, 0);
        assert!(session.downlink_out.lock().is_empty());
    }

    #[test]
    fn a_second_producer_waits_until_the_first_has_gone() {
        let (session, endpoint, delivered) = start();
        let mut first = connect(&endpoint);

        // The second connects at the TCP level but is not served: its handshake
        // runs only once the first has gone.
        let (sender, second_ready) = mpsc::channel();
        let port = endpoint.port();
        let second = thread::spawn(move || {
            let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            stream.set_read_timeout(Some(WAIT)).unwrap();
            let (mut socket, _) =
                tungstenite::client(format!("ws://127.0.0.1:{port}/"), stream).unwrap();
            sender.send(()).unwrap();
            socket.send(Message::binary(vec![2u8; 4])).unwrap();
            socket
        });
        assert!(
            second_ready
                .recv_timeout(Duration::from_millis(300))
                .is_err(),
            "the second producer is not served while the first is"
        );
        first.send(Message::binary(vec![1u8; 4])).unwrap();
        delivered.recv_timeout(WAIT).unwrap();

        first.close(None).unwrap();
        // Read until the endpoint's reply ends the connection.
        while first.read().is_ok() {}
        second_ready
            .recv_timeout(WAIT)
            .expect("served once the first has gone");
        delivered.recv_timeout(WAIT).unwrap();
        let _second = second.join().unwrap();

        assert_eq!(*session.frames.lock(), vec![vec![1u8; 4], vec![2u8; 4]]);
        assert_eq!(session.counters.snapshot().producers_connected, 2);
    }

    #[test]
    fn a_close_is_answered_and_no_data_follows_it() {
        let (session, endpoint, _) = start();
        let mut producer = connect(&endpoint);
        wait_until("the producer is connected", || {
            session.counters.snapshot().producer_connected
        });
        // Downlink traffic throughout the close, so a data frame that slipped
        // in behind the reply would be there to see.
        let flooding = Arc::new(AtomicBool::new(true));
        let flood = thread::spawn({
            let session = Arc::clone(&session);
            let flooding = Arc::clone(&flooding);
            move || {
                while flooding.load(Ordering::Relaxed) {
                    session.downlink_out.lock().push_back(vec![5u8; 64]);
                    session.wake();
                    thread::yield_now();
                }
            }
        });
        wait_until("data is flowing", || {
            session.counters.snapshot().downlink_messages_sent > 10
        });
        producer.close(None).unwrap();
        let mut closed = false;
        loop {
            match producer.read() {
                Ok(Message::Binary(_)) => assert!(!closed, "a data frame followed the close"),
                Ok(Message::Close(_)) => closed = true,
                Ok(other) => panic!("unexpected {other:?}"),
                Err(tungstenite::Error::ConnectionClosed) => break,
                Err(error) => panic!("the close handshake completes cleanly: {error}"),
            }
        }
        assert!(closed, "the endpoint answered the close");
        flooding.store(false, Ordering::Relaxed);
        flood.join().unwrap();
        wait_until("the endpoint sees the producer go", || {
            !session.counters.snapshot().producer_connected
        });
    }

    #[test]
    fn a_ping_is_answered() {
        let (_session, endpoint, _) = start();
        let mut producer = connect(&endpoint);
        producer.send(Message::Ping(vec![7u8; 3].into())).unwrap();
        match producer.read().unwrap() {
            Message::Pong(payload) => assert_eq!(payload.as_ref(), [7u8; 3]),
            other => panic!("expected a pong, got {other:?}"),
        }
    }

    #[test]
    fn a_connection_that_sends_text_is_closed_unread() {
        let (session, endpoint, _) = start();
        let mut producer = connect(&endpoint);
        producer.send(Message::text("hello")).unwrap();
        match producer.read().unwrap() {
            Message::Close(Some(CloseFrame { code, .. })) => {
                assert_eq!(code, CloseCode::Unsupported);
            }
            other => panic!("expected a close, got {other:?}"),
        }
        assert!(session.frames.lock().is_empty());
    }

    #[test]
    fn stop_clears_the_waker_and_ends_the_threads_in_every_state() {
        // Idle in accept.
        let (session, endpoint, _) = start();
        assert!(session.waker.lock().is_some());
        endpoint.stop();
        assert!(session.waker.lock().is_none());
        assert!(!endpoint.is_running());
        drop(endpoint);

        // Serving a producer.
        let (_session, endpoint, _) = start();
        let mut producer = connect(&endpoint);
        drop(endpoint);
        assert!(producer.read().is_err(), "the producer's socket is closed");

        // Holding a connection that has not sent its handshake.
        let (_session, endpoint, _) = start();
        let silent = TcpStream::connect((Ipv4Addr::LOCALHOST, endpoint.port())).unwrap();
        thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        drop(endpoint);
        assert!(
            started.elapsed() < HANDSHAKE_TIMEOUT,
            "stop does not wait out the handshake timeout"
        );
        drop(silent);
    }

    #[test]
    fn nothing_is_delivered_or_written_after_stop() {
        let (session, endpoint, _) = start();
        let mut producer = connect(&endpoint);
        wait_until("the producer is connected", || {
            session.counters.snapshot().producer_connected
        });
        endpoint.stop();
        session.downlink_out.lock().push_back(vec![1u8; 8]);
        session.wake();
        let _ = producer.send(Message::binary(vec![1u8; 8]));
        assert!(producer.read().is_err());
        drop(endpoint);
        assert!(session.frames.lock().is_empty());
        assert_eq!(session.downlink_out.lock().len(), 1, "no drain after stop");
    }
}
