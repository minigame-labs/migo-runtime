//! The handles content holds for work that outlives one call.
//!
//! A `fetch` is three of them: the request, the handle that cancels it, and
//! the response body content reads. The embedded runtime used to keep them in
//! deno_core's resource table, which the external session has no equivalent of
//! -- so the table is here, as plain ids over a map, and both executions use
//! it. The embedded ops wrap each id in a thin deno resource so `core.read`
//! still finds it in process; the external session's producer names the same
//! id on the service stream.
//!
//! Ids are per session and never reused: a stale id from content that kept one
//! past its close names nothing, rather than whatever took its place.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::ServiceError;

/// A handle content holds. Zero is never issued, so it can mean "none" on a
/// wire that has no null.
pub type ResourceId = u32;

/// What a resource is, for the error a caller sees when it names the wrong one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    FetchRequest,
    FetchCancel,
    FetchResponse,
    WebSocket,
    TcpSocket,
    UdpSocket,
}

impl ResourceKind {
    fn name(self) -> &'static str {
        match self {
            Self::FetchRequest => "fetch request",
            Self::FetchCancel => "fetch cancel handle",
            Self::FetchResponse => "fetch response",
            Self::WebSocket => "web socket",
            Self::TcpSocket => "TCP socket",
            Self::UdpSocket => "UDP socket",
        }
    }
}

/// Cancellation, as the network resources need it: a flag a reader checks and
/// a wake-up for whoever is waiting.
///
/// Dropping the last handle does not cancel; closing does. That is deliberate:
/// a response resource is held by the table and by the read in flight, and a
/// read that finished should not cancel the next one.
#[derive(Debug, Default)]
pub struct CancelFlag {
    cancelled: AtomicBool,
    woken: Notify,
}

impl CancelFlag {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.woken.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Run `work` until it finishes or this is cancelled.
    ///
    /// The wake-up is registered before the flag is read, so a cancel that
    /// lands between the two is seen rather than slept through: `cancel` sets
    /// the flag first and notifies second, so either the read sees it or the
    /// registration catches the notification.
    pub async fn until_cancelled<T>(
        &self,
        work: impl std::future::Future<Output = T>,
    ) -> Option<T> {
        let mut woken = std::pin::pin!(self.woken.notified());
        woken.as_mut().enable();
        if self.is_cancelled() {
            return None;
        }
        let work = std::pin::pin!(work);
        match futures::future::select(work, woken).await {
            futures::future::Either::Left((result, _)) => Some(result),
            futures::future::Either::Right(((), _)) => None,
        }
    }
}

/// One session's resources.
#[derive(Default)]
pub struct ResourceTable {
    next: AtomicU32,
    entries: Mutex<HashMap<ResourceId, Entry>>,
}

enum Entry {
    FetchRequest(Box<super::fetch::PendingRequest>),
    FetchCancel(Arc<CancelFlag>),
    FetchResponse(Arc<super::fetch::ResponseBody>),
    WebSocket(Arc<super::websocket::WebSocketConn>),
    TcpSocket(Arc<super::tcp::TcpConn>),
    UdpSocket(Arc<super::udp::UdpSock>),
}

impl Entry {
    fn kind(&self) -> ResourceKind {
        match self {
            Self::FetchRequest(_) => ResourceKind::FetchRequest,
            Self::FetchCancel(_) => ResourceKind::FetchCancel,
            Self::FetchResponse(_) => ResourceKind::FetchResponse,
            Self::WebSocket(_) => ResourceKind::WebSocket,
            Self::TcpSocket(_) => ResourceKind::TcpSocket,
            Self::UdpSocket(_) => ResourceKind::UdpSocket,
        }
    }
}

impl ResourceTable {
    pub fn new() -> Self {
        Self {
            next: AtomicU32::new(1),
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn add(&self, entry: Entry) -> ResourceId {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.entries.lock().insert(id, entry);
        id
    }

    pub(crate) fn add_request(&self, request: super::fetch::PendingRequest) -> ResourceId {
        self.add(Entry::FetchRequest(Box::new(request)))
    }

    pub fn add_cancel(&self, cancel: Arc<CancelFlag>) -> ResourceId {
        self.add(Entry::FetchCancel(cancel))
    }

    pub(crate) fn add_response(&self, body: Arc<super::fetch::ResponseBody>) -> ResourceId {
        self.add(Entry::FetchResponse(body))
    }

    pub(crate) fn add_web_socket(&self, connection: Arc<super::websocket::WebSocketConn>) -> ResourceId {
        self.add(Entry::WebSocket(connection))
    }

    /// The cancel flag `id` names, if it is one: an upload asks before it
    /// starts, because a handle content has already closed means it was
    /// aborted rather than that it cannot be.
    pub fn cancel_flag(&self, id: ResourceId) -> Option<Arc<CancelFlag>> {
        match self.entries.lock().get(&id) {
            Some(Entry::FetchCancel(flag)) => Some(Arc::clone(flag)),
            _ => None,
        }
    }

    /// The connection `id` names.
    pub fn web_socket(
        &self,
        id: ResourceId,
    ) -> Result<Arc<super::websocket::WebSocketConn>, ServiceError> {
        match self.entries.lock().get(&id) {
            Some(Entry::WebSocket(connection)) => Ok(Arc::clone(connection)),
            Some(other) => Err(wrong_kind(id, other.kind(), ResourceKind::WebSocket)),
            None => Err(no_such(id)),
        }
    }

    pub(crate) fn add_tcp(&self, connection: Arc<super::tcp::TcpConn>) -> ResourceId {
        self.add(Entry::TcpSocket(connection))
    }

    /// The TCP connection `id` names.
    pub fn tcp(&self, id: ResourceId) -> Result<Arc<super::tcp::TcpConn>, ServiceError> {
        match self.entries.lock().get(&id) {
            Some(Entry::TcpSocket(connection)) => Ok(Arc::clone(connection)),
            Some(other) => Err(wrong_kind(id, other.kind(), ResourceKind::TcpSocket)),
            None => Err(no_such(id)),
        }
    }

    pub(crate) fn add_udp(&self, socket: Arc<super::udp::UdpSock>) -> ResourceId {
        self.add(Entry::UdpSocket(socket))
    }

    /// The UDP socket `id` names.
    pub fn udp(&self, id: ResourceId) -> Result<Arc<super::udp::UdpSock>, ServiceError> {
        match self.entries.lock().get(&id) {
            Some(Entry::UdpSocket(socket)) => Ok(Arc::clone(socket)),
            Some(other) => Err(wrong_kind(id, other.kind(), ResourceKind::UdpSocket)),
            None => Err(no_such(id)),
        }
    }

    /// Take the request out: a send consumes it, so a second send on the same
    /// id is the error rather than a second request.
    pub(crate) fn take_request(
        &self,
        id: ResourceId,
    ) -> Result<super::fetch::PendingRequest, ServiceError> {
        let mut entries = self.entries.lock();
        match entries.remove(&id) {
            Some(Entry::FetchRequest(request)) => Ok(*request),
            Some(other) => {
                let kind = other.kind();
                entries.insert(id, other);
                Err(wrong_kind(id, kind, ResourceKind::FetchRequest))
            }
            None => Err(no_such(id)),
        }
    }

    /// The body `id` names. Public because the embedded runtime's handle
    /// answers deno's `size_hint` from it, which is how `ReadableStream`
    /// sizes a download it is about to read.
    pub fn response(
        &self,
        id: ResourceId,
    ) -> Result<Arc<super::fetch::ResponseBody>, ServiceError> {
        match self.entries.lock().get(&id) {
            Some(Entry::FetchResponse(body)) => Ok(Arc::clone(body)),
            Some(other) => Err(wrong_kind(id, other.kind(), ResourceKind::FetchResponse)),
            None => Err(no_such(id)),
        }
    }

    /// Close one resource. Answers whether it was there, for `close` (which
    /// refuses an unknown id) against `tryClose` (which does not).
    pub fn close(&self, id: ResourceId) -> bool {
        let entry = self.entries.lock().remove(&id);
        match entry {
            Some(Entry::FetchCancel(cancel)) => {
                // Closing the cancel handle is how `fetch` aborts: the send in
                // flight is what it cancels.
                cancel.cancel();
                true
            }
            Some(Entry::FetchResponse(body)) => {
                body.cancel();
                true
            }
            Some(Entry::WebSocket(connection)) => {
                // Closing is how content hangs up: whatever is in flight --
                // the read parked on the server's next message, a send -- is
                // cancelled, and the connection's halves drop with the entry.
                connection.cancel();
                true
            }
            Some(Entry::TcpSocket(connection)) => {
                connection.cancel();
                true
            }
            Some(Entry::UdpSocket(socket)) => {
                socket.cancel();
                true
            }
            Some(Entry::FetchRequest(_)) => true,
            None => false,
        }
    }

    /// How many resources are open. The session's own accounting; content
    /// never sees it.
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn no_such(id: ResourceId) -> ServiceError {
    ServiceError::generic(format!("resource {id} is not open"))
}

fn wrong_kind(id: ResourceId, found: ResourceKind, wanted: ResourceKind) -> ServiceError {
    ServiceError::generic(format!(
        "resource {id} is a {}, not a {}",
        found.name(),
        wanted.name()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_is_never_reused_and_a_closed_one_names_nothing() {
        let table = ResourceTable::new();
        let first = table.add_cancel(Arc::new(CancelFlag::default()));
        let second = table.add_cancel(Arc::new(CancelFlag::default()));
        assert_ne!(first, second);
        assert!(table.close(first));
        assert!(!table.close(first), "a second close finds nothing");
        let third = table.add_cancel(Arc::new(CancelFlag::default()));
        assert_ne!(third, first, "a closed id is not handed out again");
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn closing_a_cancel_handle_cancels_what_it_names() {
        let table = ResourceTable::new();
        let flag = Arc::new(CancelFlag::default());
        let id = table.add_cancel(Arc::clone(&flag));
        assert!(!flag.is_cancelled());
        table.close(id);
        assert!(flag.is_cancelled(), "abort() is a close of this handle");
    }

    #[test]
    fn a_resource_of_another_kind_is_named_in_the_refusal() {
        let table = ResourceTable::new();
        let id = table.add_cancel(Arc::new(CancelFlag::default()));
        let error = match table.take_request(id) {
            Ok(_) => panic!("a cancel handle is not a request"),
            Err(error) => error,
        };
        assert_eq!(
            error.message,
            format!("resource {id} is a fetch cancel handle, not a fetch request")
        );
        let error = match table.response(id) {
            Ok(_) => panic!("a cancel handle is not a response"),
            Err(error) => error,
        };
        assert_eq!(
            error.message,
            format!("resource {id} is a fetch cancel handle, not a fetch response")
        );
        // Refused, and still there: a wrong kind takes nothing.
        assert_eq!(table.len(), 1);
    }

    #[tokio::test]
    async fn work_stops_when_the_flag_is_cancelled() {
        let flag = Arc::new(CancelFlag::default());
        let waiting = flag.clone();
        let task = tokio::spawn(async move {
            waiting
                .until_cancelled(std::future::pending::<()>())
                .await
                .is_none()
        });
        tokio::task::yield_now().await;
        flag.cancel();
        assert!(task.await.unwrap(), "a cancelled wait answers nothing");
        assert!(
            flag.until_cancelled(async { 1 }).await.is_none(),
            "work started after the cancel does not run"
        );
    }
}
