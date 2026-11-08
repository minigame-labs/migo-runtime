//! Product sequential packet codecs over the existing CPU, sinc and PCM loans.
pub(crate) mod packet;
mod sniff;
pub(crate) mod vorbis;
pub(crate) mod wave;

use crate::decoder::aac::{adts::Adts, incremental::Packetizer};
use crate::streaming_transport::{Decoder, Progress, Step};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum HttpRequest {
    M4a(crate::decoder::aac::m4a::SourceRequest),
    Wave(u64),
    Mp3(u64),
    Vorbis(u64),
}
enum Codec {
    Mp3(Box<crate::streaming_mp3::Mp3StreamDecoder>),
    Wave(Box<packet::PacketStream<wave::Wave>>),
    Vorbis(Box<packet::PacketStream<vorbis::Vorbis>>),
    Adts(Box<packet::PacketStream<Adts>>),
    M4a(Box<packet::PacketStream<crate::decoder::aac::m4a::M4a>>),
}
impl Codec {
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        match self {
            Self::Mp3(d) => d.step(input, eof, pcm, cancel),
            Self::Wave(d) => d.step(input, eof, pcm, cancel),
            Self::Vorbis(d) => d.step(input, eof, pcm, cancel),
            Self::Adts(d) => d.step(input, eof, pcm, cancel),
            Self::M4a(d) => d.step(input, eof, pcm, cancel),
        }
    }
    fn timeline(&self) -> EngineResult<Option<crate::streaming::StreamTimeline>> {
        match self {
            Self::Mp3(d) => d.timeline(),
            Self::Wave(d) => d.timeline(),
            Self::Vorbis(d) => d.timeline(),
            Self::Adts(d) => d.timeline(),
            Self::M4a(d) => d.timeline(),
        }
    }
}
pub(crate) struct AutoDecoder {
    detect: sniff::Detect,
    codec: Option<Codec>,
    replay: usize,
    target: u32,
    error: Option<ErrorCode>,
    indexed: bool,
    retained_length: Option<u64>,
    retain_mp3: bool,
}
impl AutoDecoder {
    #[cfg(test)]
    pub(crate) fn vorbis_watches(&self) -> Vec<(migo_alloc_probe::DeallocationWatch, usize)> {
        match self.codec.as_ref().unwrap() {
            Codec::Vorbis(d) => d.watches(),
            _ => panic!("HTTP Vorbis owner missing"),
        }
    }
    pub(crate) fn new(target: u32) -> Self {
        Self {
            detect: sniff::Detect::default(),
            codec: None,
            replay: 0,
            target,
            error: None,
            indexed: false,
            retained_length: None,
            retain_mp3: false,
        }
    }
    pub(crate) fn file_retained(target: u32) -> Self {
        Self {
            retain_mp3: true,
            ..Self::new(target)
        }
    }
    pub(crate) fn mp3_request(&self) -> Option<u64> {
        match &self.codec {
            Some(Codec::Mp3(d)) => d.source_request(),
            _ => None,
        }
    }
    pub(crate) fn resume_mp3(&mut self, offset: u64) -> EngineResult<()> {
        match &mut self.codec {
            Some(Codec::Mp3(d)) => d.resume_at(offset),
            _ => Err(invalid("sealed MP3 decoder missing")),
        }
    }
    pub(crate) fn reposition_mp3(
        &mut self,
        target: i64,
        cutover: Option<crate::stream_control::LoopRequest>,
    ) -> symphonia_core::errors::Result<(Option<u64>, u64, bool)> {
        match &mut self.codec {
            Some(Codec::Mp3(d)) => d.reposition(target, cutover),
            _ => Err(symphonia_core::errors::Error::SeekError(
                symphonia_core::errors::SeekErrorKind::Unseekable,
            )),
        }
    }
    pub(crate) fn http(target: u32) -> Self {
        Self {
            indexed: true,
            ..Self::new(target)
        }
    }
    pub(crate) fn http_retained(target: u32, length: u64) -> Self {
        Self {
            retained_length: Some(length),
            retain_mp3: true,
            ..Self::http(target)
        }
    }
    pub(crate) fn m4a(
        &mut self,
    ) -> EngineResult<&mut packet::PacketStream<crate::decoder::aac::m4a::M4a>> {
        match &mut self.codec {
            Some(Codec::M4a(d)) => Ok(d),
            _ => Err(invalid("HTTP M4A decoder missing")),
        }
    }
    pub(crate) fn source_request(&self) -> Option<crate::decoder::aac::m4a::SourceRequest> {
        match &self.codec {
            Some(Codec::M4a(d)) => d.source_request(),
            _ => None,
        }
    }
    pub(crate) fn http_request(&self) -> Option<HttpRequest> {
        match &self.codec {
            Some(Codec::M4a(d)) => d.source_request().map(HttpRequest::M4a),
            Some(Codec::Wave(d)) => d.wave_request().map(HttpRequest::Wave),
            Some(Codec::Mp3(d)) => d.source_request().map(HttpRequest::Mp3),
            Some(Codec::Vorbis(d)) => d.vorbis_request().map(HttpRequest::Vorbis),
            _ => None,
        }
    }
    pub(crate) fn resume_wave(&mut self, offset: u64) -> EngineResult<()> {
        match &mut self.codec {
            Some(Codec::Wave(d)) => d.resume_wave(offset),
            _ => Err(invalid("HTTP WAVE decoder missing")),
        }
    }
    pub(crate) fn resume_vorbis(&mut self, offset: u64) -> EngineResult<()> {
        match &mut self.codec {
            Some(Codec::Vorbis(d)) => d.resume_vorbis(offset),
            _ => Err(invalid("HTTP Vorbis decoder missing")),
        }
    }
    pub(crate) fn reposition(
        &mut self,
        target: i64,
        cutover: Option<crate::stream_control::LoopRequest>,
    ) -> EngineResult<symphonia_core::errors::Result<(Option<HttpRequest>, u64, bool)>> {
        Ok(match &mut self.codec {
            Some(Codec::M4a(d)) => d
                .reposition(target, cutover)
                .map(|(r, s, t)| (r.map(HttpRequest::M4a), s, t)),
            Some(Codec::Wave(d)) => d
                .reposition_wave(target, cutover)
                .map(|(r, s, t)| (r.map(HttpRequest::Wave), s, t)),
            Some(Codec::Mp3(d)) => d
                .reposition(target, cutover)
                .map(|(r, s, t)| (r.map(HttpRequest::Mp3), s, t)),
            Some(Codec::Vorbis(d)) => d
                .reposition_vorbis(target, cutover)
                .map(|(r, s, t)| (r.map(HttpRequest::Vorbis), s, t)),
            _ => return Err(invalid("HTTP controlled decoder missing")),
        })
    }
    fn advance(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        if let Some(code) = self.error {
            return Err(EngineError::from_detail(code, "stream decoder retired"));
        }
        if cancel.load(Ordering::Acquire) {
            self.codec = None;
            self.error = Some(ErrorCode::Cancelled);
            return Ok(Step {
                consumed: 0,
                progress: Progress::Cancelled,
                sample_rate: 0,
                channels: 0,
            });
        }
        if let Some(codec) = &mut self.codec {
            if self.replay < 8 {
                let mut step =
                    codec.step(&self.detect.prefix[self.replay..8], false, pcm, cancel)?;
                if step.consumed > 8 - self.replay {
                    return Err(invalid("codec consumed beyond sniff replay"));
                }
                self.replay += step.consumed;
                step.consumed = 0;
                if step.progress == Progress::NeedInput {
                    step.progress = Progress::Yield;
                }
                return Ok(step);
            }
            return codec.step(input, eof, pcm, cancel);
        }
        let (consumed, kind) = self.detect.step(input, eof)?;
        if let Some(kind) = kind {
            self.codec = Some(match kind {
                sniff::Kind::Mp3 => Codec::Mp3(Box::new(if self.retain_mp3 {
                    crate::streaming_mp3::Mp3StreamDecoder::retained(self.target)
                } else {
                    crate::streaming_mp3::Mp3StreamDecoder::new(self.target)
                })),
                sniff::Kind::Wave => Codec::Wave(Box::new(match self.retained_length {
                    Some(length) => packet::PacketStream::wave(self.target, length),
                    None => packet::PacketStream::new(self.target),
                })),
                sniff::Kind::Vorbis => Codec::Vorbis(Box::new(match self.retained_length {
                    Some(length) => packet::PacketStream::vorbis(self.target, length),
                    None => packet::PacketStream::new(self.target),
                })),
                sniff::Kind::Adts => Codec::Adts(Box::new(packet::PacketStream::new(self.target))),
                sniff::Kind::M4a if self.indexed => {
                    let mut decoder = if let Some(length) = self.retained_length {
                        packet::PacketStream::file(self.target, length)
                    } else {
                        packet::PacketStream::new(self.target)
                    };
                    if self.retained_length.is_some() {
                        decoder.retain();
                    }
                    Codec::M4a(Box::new(decoder))
                }
                sniff::Kind::M4a => {
                    return Err(invalid("M4A requires the indexed streaming source"));
                }
            });
        }
        Ok(Step {
            consumed,
            progress: if self.codec.is_some() {
                Progress::Yield
            } else {
                Progress::NeedInput
            },
            sample_rate: 0,
            channels: 0,
        })
    }
}
impl Decoder for AutoDecoder {
    fn presentation(&self) -> EngineResult<Option<crate::streaming::StreamPresentation>> {
        match &self.codec {
            Some(Codec::M4a(d)) => d.presentation(),
            Some(Codec::Wave(d)) => d.presentation(),
            Some(Codec::Mp3(d)) => d.presentation(),
            Some(Codec::Vorbis(d)) => d.presentation(),
            _ => self
                .timeline()
                .map(|clock| clock.map(crate::streaming::StreamPresentation::single)),
        }
    }
    fn timeline(&self) -> EngineResult<Option<crate::streaming::StreamTimeline>> {
        self.codec.as_ref().map_or(Ok(None), Codec::timeline)
    }
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        if !pcm.is_empty() {
            return Err(invalid("stream destination must be empty"));
        }
        let result = self.advance(input, eof, pcm, cancel);
        if let Err(error) = &result {
            self.codec = None;
            self.error = Some(error.code);
            pcm.clear();
        }
        result
    }
}
fn invalid(detail: impl Into<String>) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, detail.into())
}

#[cfg(test)]
pub(crate) mod tests;
