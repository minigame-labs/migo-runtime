//! Private serializer output, never an alias of a content-owned buffer.
//! The receiver decodes borrowed wire bytes and releases them immediately.
#[cfg(test)]
use deno_core::ToV8;
use deno_core::{JsBuffer, serde_v8::V8Slice, v8};

use super::{MAX_WORKER_MESSAGE_BYTES, WorkerError, WorkerQueuePayload};

#[derive(Debug)]
pub(crate) struct WorkerWire(Storage);

#[derive(Debug)]
enum Storage {
    V8(V8Slice<u8>),
    Owned(Box<[u8]>),
}

impl WorkerWire {
    pub(super) fn from_owned(bytes: Box<[u8]>) -> Result<Self, WorkerError> {
        if bytes.is_empty() || bytes.len() > MAX_WORKER_MESSAGE_BYTES {
            return Err(WorkerError::Message(
                "Invalid private Worker wire size".into(),
            ));
        }
        Ok(Self(Storage::Owned(bytes)))
    }

    pub(super) fn decode<'a, 'i>(
        self,
        scope: &mut v8::PinScope<'a, 'i>,
    ) -> Result<v8::Local<'a, v8::Value>, deno_error::JsErrorBox> {
        // The pending JS receive owns this external input. Without accounting
        // for it, direct Rust wire avoids the ArrayBuffer registration that
        // previously informed V8's external-memory GC pressure.
        let charged = self.queued_bytes() as i64;
        scope.adjust_amount_of_external_allocated_memory(charged);
        let result = {
            let bytes: &[u8] = match &self.0 {
                Storage::V8(slice) => slice,
                Storage::Owned(bytes) => bytes,
            };
            deno_core::deserialize_for_storage(scope, bytes)
        };
        drop(self);
        // Balance the charge on success and on malformed/terminated input.
        scope.adjust_amount_of_external_allocated_memory(-charged);
        result
    }

    pub(super) fn from_private_buffer(buffer: JsBuffer) -> Result<Self, WorkerError> {
        let slice = buffer.into_parts();
        let (store, range) = slice.clone().into_parts();
        // Inspect metadata before constructing a Rust byte slice. A small view
        // of a huge allocation must not bypass the retained-memory budget.
        if store.is_shared()
            || store.is_resizable_by_user_javascript()
            || range.start != 0
            || range.end != store.byte_length()
            || range.is_empty()
            || store.byte_length() > MAX_WORKER_MESSAGE_BYTES
        {
            return Err(WorkerError::Message(
                "Invalid private Worker wire buffer".into(),
            ));
        }
        Ok(Self(Storage::V8(slice)))
    }
}

impl WorkerQueuePayload for WorkerWire {
    fn queued_bytes(&self) -> usize {
        match &self.0 {
            Storage::V8(slice) => slice.clone().into_parts().0.byte_length(),
            Storage::Owned(bytes) => bytes.len(),
        }
    }
}

#[cfg(test)]
impl<'a> ToV8<'a> for WorkerWire {
    type Error = WorkerError;

    fn to_v8<'i>(
        self,
        scope: &mut v8::PinScope<'a, 'i>,
    ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
        match self.0 {
            Storage::V8(slice) => slice
                .into_v8_local(scope)
                .map(Into::into)
                .ok_or_else(|| WorkerError::Message("Unable to adopt Worker wire buffer".into())),
            Storage::Owned(bytes) => {
                let length = bytes.len();
                let store =
                    v8::ArrayBuffer::new_backing_store_from_boxed_slice(bytes).make_shared();
                let buffer = v8::ArrayBuffer::with_backing_store(scope, &store);
                Ok(v8::Uint8Array::new(scope, buffer, 0, length)
                    .unwrap()
                    .into())
            }
        }
    }
}

#[cfg(test)]
impl From<&str> for WorkerWire {
    fn from(value: &str) -> Self {
        Self(Storage::Owned(value.as_bytes().into()))
    }
}

#[cfg(test)]
impl PartialEq for WorkerWire {
    fn eq(&self, other: &Self) -> bool {
        fn bytes(wire: &WorkerWire) -> &[u8] {
            match &wire.0 {
                Storage::V8(slice) => slice,
                Storage::Owned(bytes) => bytes,
            }
        }
        bytes(self) == bytes(other)
    }
}

#[cfg(test)]
impl Eq for WorkerWire {}
