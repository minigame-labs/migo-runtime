//! The full-body adapter is also a cursor: explicit recovery must retain its
//! original response and must not turn a completed transport error into EOF.
use super::*;
use crate::{
    isomp4_source_contracts::{Config, Fault, client, fixture, start_server},
    resampled_streaming::M4aResampled,
    streaming::stream_channel,
};
use std::sync::atomic::Ordering;
use std::time::Duration;

type D = M4aResampled;

#[tokio::test]
async fn initial_body_interruption_keeps_exact_bytes_without_reopening_http() {
    let data = fixture("1-tail.m4a");
    let server = start_server(
        data.clone(),
        Config {
            hold: Some(128),
            ..Config::default()
        },
    )
    .await;
    let client = client();
    let response = client
        .get(GateKind::AudioStream, server.url.clone())
        .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
        .send()
        .await
        .unwrap();
    let mut source = <IndexedHttp as HttpSource<D>>::new(client, response).unwrap();
    let (tx, _rx) = stream_channel();
    let state = StreamingState::new();
    let SourceRead::Data(chunk) = <IndexedHttp as Source<D>>::read(&mut source, &tx, &state)
        .await
        .unwrap()
    else {
        panic!("no prefix")
    };
    let mut got = chunk.as_ref().to_vec();
    assert_eq!(got.len(), 128);
    for _ in 0..3 {
        assert!(
            tokio::time::timeout(
                Duration::from_millis(5),
                <IndexedHttp as Source<D>>::read(&mut source, &tx, &state)
            )
            .await
            .is_err()
        );
        assert!(
            <IndexedHttp as Source<D>>::read(&mut source, &tx, &state)
                .await
                .is_err()
        );
        source.resume_after_read_interruption().unwrap();
    }
    server.release.notify_one();
    loop {
        match tokio::time::timeout(
            Duration::from_secs(1),
            <IndexedHttp as Source<D>>::read(&mut source, &tx, &state),
        )
        .await
        .unwrap()
        .unwrap()
        {
            SourceRead::Data(bytes) => got.extend_from_slice(bytes.as_ref()),
            SourceRead::End => break,
            SourceRead::Cancelled => panic!("unexpected cancellation"),
        }
    }
    assert_eq!(got, data);
    assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
    assert!(server.ranges.lock().unwrap().is_empty());
}

#[tokio::test]
async fn returned_initial_body_error_cannot_be_resumed_or_reinterpreted_as_eof() {
    let server = start_server(
        fixture("1-tail.m4a"),
        Config {
            cut: Some(128),
            ..Config::default()
        },
    )
    .await;
    let client = client();
    let response = client
        .get(GateKind::AudioStream, server.url.clone())
        .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
        .send()
        .await
        .unwrap();
    let mut source = <IndexedHttp as HttpSource<D>>::new(client, response).unwrap();
    let (tx, _rx) = stream_channel();
    let state = StreamingState::new();
    loop {
        match <IndexedHttp as Source<D>>::read(&mut source, &tx, &state).await {
            Ok(SourceRead::Data(_)) => {}
            Err(_) => break,
            _ => panic!("truncated body became EOF"),
        }
    }
    assert!(source.resume_after_read_interruption().is_err());
    assert!(
        <IndexedHttp as Source<D>>::read(&mut source, &tx, &state)
            .await
            .is_err()
    );
    assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn prepared_failed_control_reader_cannot_be_hidden_by_the_initial_response() {
    let server = start_server(
        fixture("1-tail.m4a"),
        Config {
            fault: Fault::ChangedTag,
            ..Config::default()
        },
    )
    .await;
    let client = client();
    let response = client
        .get(GateKind::AudioStream, server.url.clone())
        .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
        .send()
        .await
        .unwrap();
    let identity = range::Identity::capture(&response).unwrap();
    let mut reader = range::Reader::new(
        client.clone(),
        GateKind::AudioStream,
        identity,
        range::Limits::default(),
    );
    assert!(
        reader
            .read_exact_at(0, 8, Instant::now() + Duration::from_secs(1))
            .await
            .is_err()
    );
    let mut source = IndexedHttp::prepared(client, response, reader);
    assert!(source.resume_after_read_interruption().is_err());
    let (tx, _rx) = stream_channel();
    let state = StreamingState::new();
    assert!(
        <IndexedHttp as Source<D>>::read(&mut source, &tx, &state)
            .await
            .is_err()
    );
    assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
    assert_eq!(server.ranges.lock().unwrap().len(), 1);
}
