//! One validated full response owns its exact prefix and original byte length.
//! Draining a prefix never turns a remaining-body size hint into object identity.
use crate::{
    off_worker::OffWorker,
    source_chunk::{BodyChunk, Chunk},
    streaming::{BODY_IDLE_TIMEOUT, StreamMsg, StreamingState},
    streaming_transport::{Decoder, Source, SourceRead, SourceResume, while_active},
};
use migo_io::pools::IoPools;
use migo_services::network::http::Response;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{collections::VecDeque, sync::Arc};
use tokio::{sync::mpsc, time::Instant};
pub(crate) struct Body {
    response: Response,
    expected: Option<u64>,
    prefix: VecDeque<BodyChunk>,
    received: u64,
    eof: bool,
}
fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, e.to_string())
}
impl Body {
    pub(crate) fn new(response: Response) -> EngineResult<Self> {
        if response.decoded_body_bytes() != 0 {
            return Err(error("spool requires an unread response body"));
        }
        if response.status() != http::StatusCode::OK
            || response.headers().contains_key(http::header::CONTENT_RANGE)
        {
            return Err(error("spool requires a complete HTTP response"));
        }
        let expected = response.content_length();
        Ok(Self {
            response,
            expected,
            prefix: VecDeque::new(),
            received: 0,
            eof: false,
        })
    }
    pub(crate) fn response(&self) -> &Response {
        &self.response
    }
    pub(crate) fn prefix(&self) -> &VecDeque<BodyChunk> {
        &self.prefix
    }
    pub(crate) fn received(&self) -> u64 {
        self.received
    }
    pub(crate) fn complete(&self) -> bool {
        self.eof
    }
    pub(crate) fn at_declared_end(&self) -> bool {
        self.expected == Some(self.received)
    }
    pub(crate) async fn read_prefix(
        &mut self,
        pools: &IoPools,
        deadline: Instant,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<bool> {
        if self.eof {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Err(error("audio prefix deadline exceeded"));
        }
        let Some(chunk) = while_active(
            tx,
            state,
            tokio::time::timeout_at(
                deadline.min(Instant::now() + BODY_IDLE_TIMEOUT),
                self.response.chunk(),
            ),
        )
        .await
        else {
            return Ok(false);
        };
        let chunk = chunk
            .map_err(|_| error("audio prefix body deadline exceeded"))?
            .map_err(error)?;
        if Instant::now() >= deadline {
            return Err(error("late audio prefix completion"));
        }
        match chunk {
            Some(bytes) => {
                let chunk = BodyChunk::new(bytes, pools)?;
                self.received = self
                    .received
                    .checked_add(chunk.data.len() as u64)
                    .ok_or_else(|| error("audio prefix extent overflow"))?;
                if self.expected.is_some_and(|n| self.received > n) {
                    return Err(error("audio prefix exceeds original length"));
                }
                self.prefix.push_back(chunk);
            }
            None => {
                if self.expected.is_some_and(|n| n != self.received) {
                    return Err(error("audio prefix ended before original length"));
                }
                self.eof = true;
            }
        }
        Ok(true)
    }
    pub(crate) fn into_parts(self) -> (Response, Option<u64>, VecDeque<BodyChunk>) {
        let length = self.expected.or(self.eof.then_some(self.received));
        (self.response, length, self.prefix)
    }
}

pub(crate) struct Replayed<S> {
    pub(crate) source: S,
    pub(crate) prefix: VecDeque<BodyChunk>,
}
impl<D: Decoder> Source<D> for Replayed<crate::streaming_transport::Sequential> {
    type Request = ();
    fn pending(decoder: &D) -> EngineResult<Self::Request> {
        <crate::streaming_transport::Sequential as Source<D>>::pending(decoder)
    }
    async fn read(
        &mut self,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        if let Some(chunk) = self.prefix.pop_front() {
            return Ok(SourceRead::Data(Chunk::Prefix(chunk)));
        }
        Ok(
            match <crate::streaming_transport::Sequential as Source<D>>::read(
                &mut self.source,
                tx,
                state,
            )
            .await?
            {
                SourceRead::Data(v) => SourceRead::Data(Chunk::Tail(v)),
                SourceRead::End => SourceRead::End,
                SourceRead::Cancelled => SourceRead::Cancelled,
            },
        )
    }
    fn resume(
        &mut self,
        request: Self::Request,
        decoder: OffWorker<Option<D>>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> impl std::future::Future<Output = EngineResult<Option<SourceResume<D>>>> + Send {
        let _ = (request, tx, state);
        drop(decoder);
        async { Err(error("sequential replay cannot reposition")) }
    }
}
