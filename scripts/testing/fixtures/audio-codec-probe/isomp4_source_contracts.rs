//! Real initial HTTP body + separately pinned index, installed into the same
//! bounded decoder. PCM collection below is a test oracle, not a product cache.
use crate::network::{
    gate::GateKind,
    http::{HttpClient, HttpClientBuilder, range},
    url::Url,
};
use crate::{
    candidate,
    isomp4_source::{IndexSource, Limits, MediaRead},
    off_worker::OffWorker,
    push_decoder::{M4aDecoder, M4aDecoderMode, M4aSourceRequest, Progress, SAMPLES_PER_STEP},
    streaming::{StreamMsg, StreamingState, stream_channel},
    streaming_transport::while_active,
};
use http::{HeaderValue, header::ACCEPT_ENCODING};
use shared::network_policy::NetworkPolicy;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use symphonia_format_isomp4::streaming::{AtomKind, AtomSpan, AudioIndex, IndexOptions};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Notify, mpsc},
    time::Instant,
};

pub(crate) fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name),
    )
    .unwrap()
}
fn atom(data: &[u8], kind: AtomKind) -> AtomSpan {
    let mut position = 0;
    loop {
        let span = AtomSpan::read(&data[position..], position as u64, Some(data.len() as u64))
            .unwrap()
            .unwrap();
        if span.kind == kind {
            return span;
        }
        position = span.end.unwrap() as usize;
    }
}
pub(crate) fn client() -> HttpClient {
    HttpClientBuilder::new(
        "m4a-index-contract",
        false,
        &NetworkPolicy {
            allow_plain_http: true,
            ..NetworkPolicy::unrestricted()
        },
        "audio",
        migo_io::pools::IoPools::new(0),
    )
    .no_proxy()
    .any_address()
    .build()
    .unwrap()
}
#[derive(Clone, Copy, Default)]
pub(crate) enum Fault {
    #[default]
    None,
    ChangedTag,
    IgnoredRange,
    HangHeaders,
    HangBody,
    Slow,
    BodyShort,
}
#[derive(Clone, Default)]
pub(crate) struct Config {
    pub chunked: bool,
    pub no_tag: bool,
    pub unknown_range_total: bool,
    pub cut: Option<usize>,
    pub hold: Option<usize>,
    pub fault: Fault,
    pub media_fault: Fault,
    pub short_media: Option<u64>,
    pub hold_media_after: Option<usize>,
}
pub(crate) struct Server {
    pub url: Url,
    task: tokio::task::JoinHandle<()>,
    pub ranges: Arc<Mutex<Vec<(u64, u64)>>>,
    pub index_arrived: Arc<Notify>,
    pub full_requests: Arc<std::sync::atomic::AtomicUsize>,
    media_arrived: Arc<Notify>,
    media_held: Arc<Notify>,
    pub release: Arc<Notify>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(crate) async fn start_server(data: Vec<u8>, config: Config) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!(
        "http://localhost:{}/audio.m4a",
        listener.local_addr().unwrap().port()
    ))
    .unwrap();
    let media_span = atom(&data, AtomKind::MediaData);
    let data = Arc::new(data);
    let ranges = Arc::new(Mutex::new(Vec::new()));
    let observed = ranges.clone();
    let full_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let full_counted = full_requests.clone();
    let index_arrived = Arc::new(Notify::new());
    let signal = index_arrived.clone();
    let media_arrived = Arc::new(Notify::new());
    let media_signal = media_arrived.clone();
    let media_held = Arc::new(Notify::new());
    let held_signal = media_held.clone();
    let release = Arc::new(Notify::new());
    let gate = release.clone();
    let task = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let data = data.clone();
            let config = config.clone();
            let observed = observed.clone();
            let full_counted = full_counted.clone();
            let signal = signal.clone();
            let media_signal = media_signal.clone();
            let held_signal = held_signal.clone();
            let gate = gate.clone();
            children.spawn(async move {
                let mut raw=Vec::new();
                while !raw.ends_with(b"\r\n\r\n") {
                    match socket.read_u8().await { Ok(b)=>raw.push(b),Err(_)=>return }
                    assert!(raw.len()<8192);
                }
                let request=String::from_utf8(raw).unwrap().to_ascii_lowercase();
                assert!(request.contains("accept-encoding: identity\r\n"));
                let range=request.lines().find_map(|line|line.strip_prefix("range: bytes="));
                let tag=if config.no_tag {""} else {"ETag: \"asset-v1\"\r\n"};
                if let Some(range)=range {
                    assert!(request.contains("if-range: \"asset-v1\"\r\n"));
                    let (a,b)=range.trim().split_once('-').unwrap();
                    let a=a.parse::<u64>().unwrap();let mut b=b.parse::<u64>().unwrap();
                    let media = a >= media_span.body_offset && a < media_span.end.unwrap() && b == data.len() as u64 - 1;
                    let fault = if media && !matches!(config.media_fault,Fault::None) { config.media_fault } else { config.fault };
                    if media {
                        if let Some(limit) = config.short_media { b=b.min(a+limit-1); }
                        media_signal.notify_one();
                    }
                    observed.lock().unwrap().push((a,b));signal.notify_one();
                    let hold = if media && observed.lock().unwrap().iter().filter(|(a,b)| *a>=media_span.body_offset && *a<media_span.end.unwrap() && *b==data.len() as u64-1).count()==1 { config.hold_media_after } else { None };
                    if matches!(fault,Fault::HangHeaders) { std::future::pending::<()>().await; }
                    if matches!(fault,Fault::Slow) { tokio::time::sleep(Duration::from_millis(30)).await; }
                    if matches!(fault,Fault::IgnoredRange) {
                        let _=socket.write_all(format!("HTTP/1.1 200 OK\r\n{tag}Content-Length: {}\r\nConnection: close\r\n\r\n",data.len()).as_bytes()).await;
                        std::future::pending::<()>().await;
                    }
                    let tag=if matches!(fault,Fault::ChangedTag) {"ETag: \"asset-v2\"\r\n"} else {tag};
                    let total=if config.unknown_range_total {"*".to_owned()} else {data.len().to_string()};
                    let chunked=matches!(fault,Fault::BodyShort) || (media && config.hold_media_after.is_some());
                    let framing=if chunked { "Transfer-Encoding: chunked\r\n".to_owned() } else { format!("Content-Length: {}\r\n",b-a+1) };
                    let head=format!("HTTP/1.1 206 Partial Content\r\n{tag}Content-Range: bytes {a}-{b}/{total}\r\n{framing}Connection: close\r\n\r\n");
                    if socket.write_all(head.as_bytes()).await.is_err() { return; }
                    if matches!(fault,Fault::HangBody) { std::future::pending::<()>().await; }
                    if b as usize>=data.len() { return; }
                    let end=b as usize + 1 - usize::from(matches!(fault,Fault::BodyShort));
                    let mut at=a as usize;
                    while at<end {
                        let until=(at+137).min(end).min(if media { hold.map(|n|a as usize+n).filter(|n|*n>at).unwrap_or(end) } else { end });
                        let bytes=&data[at..until];
                        if chunked && socket.write_all(format!("{:x}\r\n",bytes.len()).as_bytes()).await.is_err(){return;}
                        if socket.write_all(bytes).await.is_err(){return;}
                        if chunked && socket.write_all(b"\r\n").await.is_err(){return;}
                        at=until;
                        if media && hold==Some(at-a as usize) { held_signal.notify_one(); gate.notified().await; }
                    }
                    if chunked { let _=socket.write_all(b"0\r\n\r\n").await; }
                } else {
                    full_counted.fetch_add(1, std::sync::atomic::Ordering::Release);
                    let cut=config.cut.unwrap_or(data.len());
                    let framing=if config.chunked {"Transfer-Encoding: chunked\r\n".to_owned()} else {format!("Content-Length: {}\r\n",data.len())};
                    let head=format!("HTTP/1.1 200 OK\r\n{tag}{framing}Connection: close\r\n\r\n");
                    if socket.write_all(head.as_bytes()).await.is_err() { return; }
                    let mut position=0;
                    while position<cut {
                        if config.hold==Some(position) { gate.notified().await; }
                        let end=(position+4096).min(cut).min(config.hold.filter(|h|*h>position).unwrap_or(cut));
                        let bytes=&data[position..end];
                        if config.chunked && socket.write_all(format!("{:x}\r\n",bytes.len()).as_bytes()).await.is_err(){return;}
                        if socket.write_all(bytes).await.is_err(){return;}
                        if config.chunked && socket.write_all(b"\r\n").await.is_err(){return;}
                        position=end;
                    }
                    if config.chunked { let _=socket.write_all(b"0\r\n\r\n").await; }
                }
            });
        }
    });
    Server {
        url,
        task,
        ranges,
        index_arrived,
        full_requests,
        media_arrived,
        media_held,
        release,
    }
}

struct Decoded {
    pcm: Vec<f32>,
    indexes: usize,
    index_bytes: u64,
    resumes: u32,
    media_requests: u64,
    media_bytes: u64,
    buffer: (usize, usize, u64, u64),
    first_source_peak: Option<usize>,
}
async fn decode(
    client: HttpClient,
    url: Url,
    fragment: usize,
    limits: Limits,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    first_pcm: Arc<Notify>,
    index_timeout: Duration,
) -> Result<Option<Decoded>, String> {
    decode_mode::<false>(
        client,
        url,
        fragment,
        limits,
        tx,
        state,
        first_pcm,
        index_timeout,
    )
    .await
}
async fn decode_mode<const BUFFERED: bool>(
    client: HttpClient,
    url: Url,
    fragment: usize,
    mut limits: Limits,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    first_pcm: Arc<Notify>,
    index_timeout: Duration,
) -> Result<Option<Decoded>, String> {
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let io_thread = std::thread::current().id();
    let Some(response) = while_active(
        &tx,
        &state,
        client
            .get(GateKind::AudioStream, url)
            .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
            .send(),
    )
    .await
    else {
        return Ok(None);
    };
    let response = response.map_err(|e| e.to_string())?;
    let length = response.content_length();
    let mut identity = Some(range::Identity::capture(&response));
    limits.index.object_len = length;
    let mut source = None::<IndexSource>;
    let mut decoder = OffWorker::new(
        M4aDecoderMode::<BUFFERED>::with_object_len(None, length),
        owner.audio_cpu(),
    );
    let mut pcm = Vec::new();
    let mut scratch = Vec::with_capacity(SAMPLES_PER_STEP);
    let mut indexes = 0;
    let mut first_source_peak = None;
    let mut eof_sent = false;
    let mut response = Some(response);
    let mut using_media = false;
    let mut pending_chunk = None;
    'download: loop {
        let chunk = if let Some(bytes) = pending_chunk.take() {
            Some(bytes)
        } else if using_media {
            match source
                .as_mut()
                .unwrap()
                .media_chunk(&tx, &state)
                .await
                .map_err(|e| e.to_string())?
            {
                MediaRead::Data(bytes) => Some(bytes),
                MediaRead::End => None,
                MediaRead::Cancelled => return Ok(None),
            }
        } else {
            let Some(chunk) = while_active(&tx, &state, response.as_mut().unwrap().chunk()).await
            else {
                return Ok(None);
            };
            chunk.map_err(|e| e.to_string())?
        };
        let eof = chunk.is_none();
        let mut offset = 0;
        loop {
            let input = chunk.clone();
            let active = state.clone();
            let operation = decoder.with(move |value| {
                assert_ne!(
                    std::thread::current().id(),
                    io_thread,
                    "decode must run on the common CPU lane"
                );
                scratch.clear();
                let accepted = if eof {
                    if !eof_sent {
                        value.finish_input();
                    }
                    0
                } else {
                    let bytes = input.as_ref().unwrap();
                    let end = (offset + fragment).min(bytes.len());
                    value.push(&bytes[offset..end])?
                };
                let progress = value.drain(&mut scratch, &active.cancelled)?;
                Ok::<_, String>((
                    scratch,
                    accepted,
                    progress,
                    value.source_request(),
                    value.buffered_metrics(),
                ))
            });
            let Some(result) = while_active(&tx, &state, operation).await else {
                return Ok(None);
            };
            let (returned, outcome) = result.map_err(|e| e.to_string())?;
            decoder = returned;
            let (out, accepted, progress, request, buffer) = outcome?;
            scratch = out;
            offset += accepted;
            if eof {
                eof_sent = true;
            }
            if !scratch.is_empty() && pcm.is_empty() {
                first_pcm.notify_one();
            }
            pcm.extend_from_slice(&scratch);
            match progress {
                Progress::NeedSource => {
                    first_source_peak.get_or_insert(buffer.0);
                    if source.is_none() {
                        let pin = identity.take().unwrap().map_err(|e| e.to_string())?;
                        source = Some(IndexSource::new(
                            range::Reader::new(
                                client.clone(),
                                GateKind::AudioStream,
                                pin,
                                range::Limits::default(),
                            ),
                            limits,
                        ));
                    }
                    match request.ok_or("missing source request")? {
                        M4aSourceRequest::Index(request) => {
                            let result = source
                                .as_mut()
                                .unwrap()
                                .resolve(
                                    request,
                                    decoder,
                                    &tx,
                                    &state,
                                    Instant::now() + index_timeout,
                                )
                                .await
                                .map_err(|e| e.to_string())?;
                            let Some(result) = result else {
                                return Ok(None);
                            };
                            assert_ne!(
                                result.parse_thread, io_thread,
                                "full moov parsing ran on the IO thread"
                            );
                            decoder = result.decoder;
                            indexes += 1;
                        }
                        M4aSourceRequest::ReadAt(packet) => {
                            let result = source
                                .as_mut()
                                .unwrap()
                                .read_at(
                                    packet,
                                    decoder,
                                    &tx,
                                    &state,
                                    Instant::now() + index_timeout,
                                )
                                .await
                                .map_err(|e| e.to_string())?;
                            let Some(result) = result else {
                                return Ok(None);
                            };
                            assert_ne!(
                                result.resume_thread, io_thread,
                                "decoder resume ran on the IO thread"
                            );
                            decoder = result.decoder;
                            pending_chunk = Some(result.first);
                            using_media = true;
                            response.take();
                            eof_sent = false;
                            continue 'download;
                        }
                    }
                }
                Progress::Yield => {}
                Progress::NeedInput => {
                    if eof {
                        return Err("EOF requested more bytes".into());
                    }
                    if offset == chunk.as_ref().unwrap().len() {
                        break;
                    }
                }
                Progress::Finished => {
                    if !eof || pcm.is_empty() {
                        return Err("incomplete HTTP/PCM finish".into());
                    }
                    return Ok(Some(Decoded {
                        pcm,
                        indexes,
                        index_bytes: source.as_ref().map_or(0, IndexSource::bytes_received),
                        resumes: source.as_ref().map_or(0, IndexSource::resumes),
                        media_requests: source.as_ref().map_or(0, IndexSource::media_requests),
                        media_bytes: source.as_ref().map_or(0, IndexSource::media_bytes_received),
                        buffer,
                        first_source_peak,
                    }));
                }
                Progress::Cancelled => return Ok(None),
            }
        }
    }
}
async fn run(server: &Server, fragment: usize, limits: Limits) -> Result<Decoded, String> {
    let (tx, _rx) = stream_channel();
    tokio::time::timeout(
        Duration::from_secs(10),
        decode(
            client(),
            server.url.clone(),
            fragment,
            limits,
            tx,
            StreamingState::new(),
            Arc::new(Notify::new()),
            Duration::from_secs(2),
        ),
    )
    .await
    .expect("index/HTTP decode must finish")
    .map(|r| r.expect("unexpected cancellation"))
}

async fn pinned(server: &Server, limits: Limits) -> IndexSource {
    let client = client();
    let response = client
        .get(GateKind::AudioStream, server.url.clone())
        .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
        .send()
        .await
        .unwrap();
    let identity = range::Identity::capture(&response).unwrap();
    IndexSource::new(
        range::Reader::new(
            client,
            GateKind::AudioStream,
            identity,
            range::Limits::default(),
        ),
        limits,
    )
}

async fn pending(
    data: Vec<u8>,
    owner: &migo_io::scheduler::IoScheduler,
) -> (
    OffWorker<M4aDecoder>,
    symphonia_format_isomp4::streaming::IndexedPacket,
) {
    let decoder = OffWorker::new(
        M4aDecoder::with_object_len(None, Some(data.len() as u64)),
        owner.audio_cpu(),
    );
    let (decoder, packet) = decoder
        .with(move |value| {
            let mut at = 0;
            let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
            loop {
                if at < data.len() {
                    at += value.push(&data[at..]).unwrap();
                } else {
                    value.finish_input();
                }
                out.clear();
                match value
                    .drain(&mut out, &std::sync::atomic::AtomicBool::new(false))
                    .unwrap()
                {
                    Progress::NeedSource => match value.source_request().unwrap() {
                        M4aSourceRequest::ReadAt(packet) => return packet,
                        M4aSourceRequest::Index(_) => panic!("helper requires a front movie"),
                    },
                    Progress::Finished => panic!("fixture never requested a reposition"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
    (decoder, packet)
}

#[tokio::test]
async fn actual_read_at_reuses_media_extents_and_preserves_aac_and_container_end() {
    let mut count = 0;
    for name in [
        "1-reorder.m4a",
        "2-reorder.m4a",
        "2-reverse.m4a",
        "2-multi.m4a",
        "2-tail-reverse.m4a",
        "2-tail-multi.m4a",
    ] {
        let mut data = fixture(name);
        data.extend_from_slice(&1032_u32.to_be_bytes());
        data.extend_from_slice(b"free");
        data.extend(std::iter::repeat_n(0x5a, 1024));
        let expected = candidate::decode(&data, 16_777_216).unwrap().samples;
        for chunked in [false, true] {
            for fragment in [137, 8192] {
                let server = start_server(
                    data.clone(),
                    Config {
                        chunked,
                        ..Config::default()
                    },
                )
                .await;
                let result = run(&server, fragment, Limits::default()).await.unwrap();
                assert_eq!(result.pcm, expected, "{name}/{chunked}/{fragment}");
                let resumes = if name.contains("reorder") { 1 } else { 2 };
                assert_eq!(result.resumes, resumes);
                assert_eq!(result.media_requests, u64::from(resumes));
                assert!(result.media_bytes > 0);
                let control: Vec<_> = server
                    .ranges
                    .lock()
                    .unwrap()
                    .iter()
                    .copied()
                    .filter(|(_, b)| *b != data.len() as u64 - 1)
                    .collect();
                // Front layouts discover at most five headers once. Both
                // earlier mdats are then cached even across two backwards reads.
                if !name.contains("tail") {
                    assert!(
                        control.len() <= 5,
                        "media headers were rediscovered: {control:?}"
                    );
                }
                count += 1;
            }
        }
    }
    assert_eq!(count, 24);
    println!("W09_M4A_HTTP_READ_AT {count} exact PCM comparisons");
}

#[tokio::test]
async fn actual_read_at_short_ranges_and_held_body_deliver_before_tail_release() {
    let data = fixture("2-reverse.m4a");
    let expected = candidate::decode(&data, 16_777_216).unwrap().samples;
    let server = start_server(
        data.clone(),
        Config {
            short_media: Some(4096),
            ..Config::default()
        },
    )
    .await;
    let result = run(&server, 137, Limits::default()).await.unwrap();
    assert_eq!(result.pcm, expected);
    assert!(result.media_requests > u64::from(result.resumes));
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let (_, packet) = pending(data.clone(), &owner).await;
    assert!(u64::from(packet.size) < data.len() as u64 - packet.offset);
    let server = start_server(
        data,
        Config {
            hold_media_after: Some(packet.size as usize),
            ..Config::default()
        },
    )
    .await;
    let (tx, _rx) = stream_channel();
    let first = Arc::new(Notify::new());
    let job = decode(
        client(),
        server.url.clone(),
        137,
        Limits::default(),
        tx,
        StreamingState::new(),
        first.clone(),
        Duration::from_secs(2),
    );
    tokio::pin!(job);
    tokio::select! {result=&mut job=>panic!("held media completed early: {}",result.is_ok()),
    _=first.notified()=>{}, _=tokio::time::sleep(Duration::from_secs(3))=>panic!("firstPCM waited for mediaEOF")}
    tokio::time::timeout(Duration::from_millis(500), server.media_held.notified())
        .await
        .expect("server must have entered the actual media hold before firstPCM");
    server.release.notify_one();
    let result = job.await.unwrap().unwrap();
    assert_eq!(result.pcm, expected);
}

#[tokio::test]
async fn read_at_rejects_nonmedia_packets_limits_and_unknown_complete_lengths() {
    let data = fixture("2-reverse.m4a");
    let owner = migo_io::scheduler::IoScheduler::new(0);
    for bad in [0, 1, 2, 3, 4, 5] {
        let server = start_server(data.clone(), Config::default()).await;
        let limits = match bad {
            2 => Limits {
                max_resumes: 0,
                ..Limits::default()
            },
            3 => Limits {
                max_atoms: 1,
                ..Limits::default()
            },
            _ => Limits::default(),
        };
        let mut source = pinned(&server, limits).await;
        let (decoder, mut packet) = pending(data.clone(), &owner).await;
        if bad == 0 {
            packet.offset = 8;
            packet.size = 1;
        }
        if bad == 4 {
            packet.offset = atom(&data, AtomKind::MediaData).offset + 1;
            packet.size = 1;
        }
        if bad == 5 {
            packet.size = 0;
        }
        if bad == 1 {
            packet.size = data.len() as u32;
        }
        let (tx, _rx) = stream_channel();
        assert!(
            source
                .read_at(
                    packet,
                    decoder,
                    &tx,
                    &StreamingState::new(),
                    Instant::now() + Duration::from_secs(2)
                )
                .await
                .is_err()
        );
        assert!(
            server
                .ranges
                .lock()
                .unwrap()
                .iter()
                .all(|(a, b)| b - a + 1 <= 16),
            "bad packet triggered a media body fetch"
        );
    }
    let server = start_server(
        data,
        Config {
            chunked: true,
            unknown_range_total: true,
            ..Config::default()
        },
    )
    .await;
    let result = run(&server, 8192, Limits::default()).await;
    assert!(result.err().is_some_and(|e| e.contains("UnknownLength")));
    let server = start_server(
        fixture("2-reverse.m4a"),
        Config {
            no_tag: true,
            ..Config::default()
        },
    )
    .await;
    assert!(
        run(&server, 137, Limits::default())
            .await
            .err()
            .is_some_and(|e| e.contains("NoStrongValidator"))
    );
    assert!(server.ranges.lock().unwrap().is_empty());
}

#[tokio::test]
async fn read_at_cannot_resume_a_different_pending_command() {
    let data = fixture("2-reverse.m4a");
    let server = start_server(data.clone(), Config::default()).await;
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let (decoder, mut packet) = pending(data, &owner).await;
    packet.track_id ^= 0x8000_0000;
    let mut source = pinned(&server, Limits::default()).await;
    let (tx, _rx) = stream_channel();
    let result = source
        .read_at(
            packet,
            decoder,
            &tx,
            &StreamingState::new(),
            Instant::now() + Duration::from_secs(2),
        )
        .await;
    assert!(result.err().is_some_and(|e| matches!(
        e,
        crate::isomp4_source::Error::Invalid("pending packet request changed")
    )));
}

#[tokio::test]
async fn read_at_version_errors_and_truncated_media_never_finish_successfully() {
    for (fault, expected) in [
        (Fault::ChangedTag, "RepresentationChanged"),
        (Fault::IgnoredRange, "RangeUnsupported"),
        (Fault::BodyShort, "truncated"),
    ] {
        let mut data = fixture("2-tail-multi.m4a");
        data.extend_from_slice(&1032_u32.to_be_bytes());
        data.extend_from_slice(b"free");
        data.extend(std::iter::repeat_n(0x5a, 1024));
        let server = start_server(
            data,
            Config {
                media_fault: fault,
                ..Config::default()
            },
        )
        .await;
        let result = run(&server, 8192, Limits::default()).await;
        assert!(
            result.err().is_some_and(|e| e.contains(expected)),
            "{expected}"
        );
    }
}

#[tokio::test]
async fn read_at_cancellation_and_receiver_exit_interrupt_real_media_waits() {
    for fault in [Fault::HangHeaders, Fault::HangBody] {
        for receiver_exit in [false, true] {
            let server = start_server(
                fixture("2-reverse.m4a"),
                Config {
                    media_fault: fault,
                    ..Config::default()
                },
            )
            .await;
            let (tx, rx) = stream_channel();
            let state = StreamingState::new();
            let job = decode(
                client(),
                server.url.clone(),
                137,
                Limits::default(),
                tx,
                state.clone(),
                Arc::new(Notify::new()),
                Duration::from_secs(2),
            );
            tokio::pin!(job);
            tokio::select! {result=&mut job=>panic!("media wait completed: {}",result.is_ok()),
            _=server.media_arrived.notified()=>{}}
            if receiver_exit {
                drop(rx);
            } else {
                state.cancel();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(500), &mut job)
                    .await
                    .expect("media cancellation must release IO")
                    .unwrap()
                    .is_none()
            );
        }
    }
}

#[tokio::test]
async fn read_at_cpu_resume_wait_shares_the_source_deadline() {
    let data = fixture("2-reverse.m4a");
    let server = start_server(data.clone(), Config::default()).await;
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let (decoder, packet) = pending(data, &owner).await;
    let mut source = pinned(
        &server,
        Limits {
            resume_delay: Duration::from_millis(750),
            ..Limits::default()
        },
    )
    .await;
    let (tx, _rx) = stream_channel();
    let start = Instant::now();
    let result = source
        .read_at(
            packet,
            decoder,
            &tx,
            &StreamingState::new(),
            start + Duration::from_millis(100),
        )
        .await;
    assert!(
        result.err().is_some_and(|e| matches!(
            e,
            crate::isomp4_source::Error::Range(range::Error::Timeout)
        ))
    );
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "IO waited for a late CPU job"
    );
}

#[tokio::test]
async fn read_at_pending_media_uses_the_absolute_setup_deadline() {
    for fault in [Fault::HangHeaders, Fault::HangBody] {
        let data = fixture("2-reverse.m4a");
        let server = start_server(
            data.clone(),
            Config {
                media_fault: fault,
                ..Config::default()
            },
        )
        .await;
        let owner = migo_io::scheduler::IoScheduler::new(0);
        let (decoder, packet) = pending(data, &owner).await;
        let mut source = pinned(&server, Limits::default()).await;
        let (tx, _rx) = stream_channel();
        let start = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            source.read_at(
                packet,
                decoder,
                &tx,
                &StreamingState::new(),
                start + Duration::from_millis(70),
            ),
        )
        .await
        .expect("source setup must not wait for the media idle timeout");
        assert!(result.err().is_some_and(|e| matches!(
            e,
            crate::isomp4_source::Error::Range(range::Error::Timeout)
        )));
    }
}
fn with_padding(mut data: Vec<u8>, count: usize, body: usize, extended: bool) -> Vec<u8> {
    let at = atom(&data, AtomKind::Movie).offset as usize;
    let mut padding = Vec::new();
    for _ in 0..count {
        if extended {
            padding.extend_from_slice(&1_u32.to_be_bytes());
            padding.extend_from_slice(b"free");
            padding.extend_from_slice(&((body + 16) as u64).to_be_bytes());
        } else {
            padding.extend_from_slice(&((body + 8) as u32).to_be_bytes());
            padding.extend_from_slice(b"free");
        }
        padding.extend(std::iter::repeat_n(0x5a, body));
    }
    data.splice(at..at, padding);
    data
}

#[tokio::test]
async fn real_http_tail_indexes_install_on_cpu_and_keep_fragmented_pcm() {
    let mut count = 0;
    for (name, fragment) in [
        ("0-tail.m4a", 1),
        ("1-tail.m4a", 137),
        ("2-tail.m4a", 8192),
        ("1-front.m4a", 137),
        ("2-co64.m4a", 8192),
        ("2-split.m4a", 8192),
        ("1-metadata.m4a", 8192),
    ] {
        let data = fixture(name);
        let expected = candidate::decode(&data, 16_777_216).unwrap().samples;
        for chunked in [false, true] {
            let server = start_server(
                data.clone(),
                Config {
                    chunked,
                    ..Config::default()
                },
            )
            .await;
            let result = run(&server, fragment, Limits::default()).await.unwrap();
            assert_eq!(result.pcm, expected, "{name}/{chunked}");
            let tail = name.contains("tail") || name == "1-metadata.m4a";
            assert_eq!(result.indexes, usize::from(tail));
            if name == "2-tail.m4a" {
                assert!(result.index_bytes < data.len() as u64 / 4);
            }
            count += 1;
        }
    }
    for extended in [false, true] {
        let data = with_padding(fixture("1-tail.m4a"), 3, 32, extended);
        let expected = candidate::decode(&data, 16_777_216).unwrap().samples;
        let server = start_server(data, Config::default()).await;
        assert_eq!(
            run(&server, 137, Limits::default()).await.unwrap().pcm,
            expected
        );
        count += 1;
    }
    assert_eq!(count, 16);
    println!("W09_M4A_HTTP_INDEX {count} exact PCM comparisons");
}
#[tokio::test]
async fn tail_index_produces_pcm_before_initial_server_releases_remaining_body() {
    let data = fixture("2-tail.m4a");
    let media = atom(&data, AtomKind::MediaData);
    let server = start_server(
        data.clone(),
        Config {
            chunked: true,
            hold: Some(media.body_offset as usize + 8192),
            ..Config::default()
        },
    )
    .await;
    let (tx, _rx) = stream_channel();
    let first = Arc::new(Notify::new());
    let notice = first.notified();
    let job = decode(
        client(),
        server.url.clone(),
        137,
        Limits::default(),
        tx,
        StreamingState::new(),
        first.clone(),
        Duration::from_secs(2),
    );
    tokio::pin!(job);
    tokio::select! {result=&mut job=>panic!("decoder waited for/reached EOF too early: {}",result.is_ok()),
    _=notice=>{},_=tokio::time::sleep(Duration::from_secs(3))=>panic!("first PCM waited for full response")}
    server.release.notify_one();
    let result = job.await.unwrap().unwrap();
    assert_eq!(
        result.pcm,
        candidate::decode(&data, 16_777_216).unwrap().samples
    );
}
#[tokio::test]
async fn actual_source_metadata_atom_and_announced_index_budgets_fail_before_body_fetch() {
    for (data, limits, needle) in [
        (
            with_padding(fixture("1-tail.m4a"), 40, 0, false),
            Limits::default(),
            "atom budget",
        ),
        (
            with_padding(fixture("1-tail.m4a"), 1, 8192, false),
            Limits {
                max_skipped_bytes: 16,
                ..Limits::default()
            },
            "metadata budget",
        ),
        (
            fixture("1-metadata.m4a"),
            Limits {
                index: IndexOptions {
                    max_index_bytes: 65536,
                    ..IndexOptions::default()
                },
                ..Limits::default()
            },
            "index byte budget",
        ),
    ] {
        let server = start_server(data, Config::default()).await;
        let error = run(&server, 8192, limits)
            .await
            .err()
            .expect("source budget must reject");
        assert!(error.contains(needle), "{error}");
        assert!(
            server
                .ranges
                .lock()
                .unwrap()
                .iter()
                .all(|(a, b)| b - a + 1 <= 16),
            "body fetched before source budget rejection"
        );
    }
}
#[tokio::test]
async fn missing_validators_and_ignored_or_changed_ranges_remain_errors() {
    for config in [
        Config {
            no_tag: true,
            ..Config::default()
        },
        Config {
            fault: Fault::IgnoredRange,
            ..Config::default()
        },
        Config {
            fault: Fault::ChangedTag,
            ..Config::default()
        },
    ] {
        let server = start_server(fixture("1-tail.m4a"), config).await;
        assert!(run(&server, 137, Limits::default()).await.is_err());
    }
    let server = start_server(
        fixture("1-front.m4a"),
        Config {
            no_tag: true,
            ..Config::default()
        },
    )
    .await;
    assert!(run(&server, 137, Limits::default()).await.is_ok());
    assert!(
        server.ranges.lock().unwrap().is_empty(),
        "front moov must not require an external validator"
    );
}
#[tokio::test]
async fn index_wait_cancellation_and_receiver_exit_release_real_pending_requests() {
    for fault in [Fault::HangHeaders, Fault::HangBody] {
        for receiver_exit in [false, true] {
            let server = start_server(
                fixture("1-tail.m4a"),
                Config {
                    fault,
                    ..Config::default()
                },
            )
            .await;
            let (tx, rx) = stream_channel();
            let state = StreamingState::new();
            let job = decode(
                client(),
                server.url.clone(),
                137,
                Limits::default(),
                tx,
                state.clone(),
                Arc::new(Notify::new()),
                Duration::from_secs(2),
            );
            tokio::pin!(job);
            tokio::select! {result=&mut job=>panic!("pending range unexpectedly finished: {}",result.is_ok()),
            _=server.index_arrived.notified()=>{}}
            if receiver_exit {
                drop(rx);
            } else {
                state.cancel();
            }
            assert!(
                tokio::time::timeout(Duration::from_secs(1), job)
                    .await
                    .expect("source wait ignored cancellation/receiver exit")
                    .unwrap()
                    .is_none()
            );
            assert!(state.is_cancelled());
        }
    }
}
#[tokio::test]
async fn learned_length_and_external_movie_eof_do_not_accept_incomplete_initial_body() {
    let mut data = fixture("1-tail.m4a");
    let before = data.len();
    data.extend_from_slice(&40_u32.to_be_bytes());
    data.extend_from_slice(b"free");
    data.extend_from_slice(&[0; 32]);
    let server = start_server(
        data,
        Config {
            chunked: true,
            cut: Some(before),
            ..Config::default()
        },
    )
    .await;
    assert!(
        run(&server, 8192, Limits::default()).await.is_err(),
        "learned length was not enforced at source EOF"
    );
    let data = fixture("1-tail.m4a");
    let cut = atom(&data, AtomKind::MediaData).end.unwrap() as usize;
    let server = start_server(
        data,
        Config {
            chunked: true,
            unknown_range_total: true,
            cut: Some(cut),
            ..Config::default()
        },
    )
    .await;
    assert!(
        run(&server, 8192, Limits::default()).await.is_err(),
        "external index hid missing physical movie atom"
    );
}
#[tokio::test]
async fn index_discovery_keeps_one_absolute_deadline_for_all_control_reads() {
    let data = with_padding(fixture("1-tail.m4a"), 4, 8, false);
    let server = start_server(
        data,
        Config {
            fault: Fault::Slow,
            ..Config::default()
        },
    )
    .await;
    let (tx, _rx) = stream_channel();
    let result = decode(
        client(),
        server.url.clone(),
        137,
        Limits::default(),
        tx,
        StreamingState::new(),
        Arc::new(Notify::new()),
        Duration::from_millis(70),
    )
    .await;
    assert!(
        result.err().is_some_and(|e| e.contains("Timeout")),
        "discovery renewed its deadline per atom"
    );
    // Every response waits 30 ms. A 70 ms absolute deadline can start at most
    // three serial reads, even if a later CPU-stage timeout would also fail.
    assert!(
        server.ranges.lock().unwrap().len() <= 3,
        "control reads continued after their deadline"
    );
}

#[tokio::test]
async fn cpu_index_wait_obeys_the_same_deadline_without_waiting_for_the_job() {
    let server = start_server(fixture("1-tail.m4a"), Config::default()).await;
    let (tx, _rx) = stream_channel();
    let start = Instant::now();
    let result = decode(
        client(),
        server.url.clone(),
        137,
        Limits {
            parse_delay: Duration::from_millis(750),
            ..Limits::default()
        },
        tx,
        StreamingState::new(),
        Arc::new(Notify::new()),
        Duration::from_millis(100),
    )
    .await;
    assert!(result.err().is_some_and(|e| e.contains("Timeout")));
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "IO waited for the late CPU result"
    );
}

#[test]
fn learned_library_length_is_monotonic_and_only_binds_pending_index() {
    use symphonia_format_isomp4::streaming::{PacketStep, Packetizer, PacketizerOptions};
    let data = fixture("1-tail.m4a");
    let len = data.len() as u64;
    let waiting = |known| {
        let mut parser = Packetizer::new(PacketizerOptions {
            index: IndexOptions {
                object_len: known,
                ..IndexOptions::default()
            },
            ..PacketizerOptions::default()
        });
        let mut at = 0;
        loop {
            match parser.next(&data[at..], true).unwrap() {
                PacketStep::Discard(n) => at += n,
                PacketStep::NeedIndex(_) => return parser,
                _ => panic!("expected pending index"),
            }
        }
    };
    let mut parser = Packetizer::new(PacketizerOptions::default());
    assert!(parser.bind_object_len(len).is_err());
    assert!(parser.next(&data, true).is_err());
    let mut parser = waiting(Some(len));
    assert!(parser.bind_object_len(len + 1).is_err());
    assert!(parser.next(&data, true).is_err());
    let mut parser = waiting(None);
    assert!(parser.bind_object_len(1).is_err());
    let mut parser = waiting(None);
    parser.bind_object_len(len).unwrap();
    parser.bind_object_len(len).unwrap();
    let movie = atom(&data, AtomKind::Movie);
    let index = AudioIndex::from_moov(
        &data[movie.offset as usize..movie.end.unwrap() as usize],
        IndexOptions {
            object_len: Some(len),
            ..IndexOptions::default()
        },
    )
    .unwrap();
    parser.install_index(index).unwrap();
    assert!(parser.bind_object_len(len).is_err());
}

#[path = "isomp4_buffered_contracts.rs"]
mod buffered;

#[cfg(feature = "m4a-transport-probe")]
#[path = "isomp4_transport_contracts.rs"]
pub(crate) mod transport;
