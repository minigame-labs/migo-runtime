//! Soft seek controls may interrupt a wait without changing the byte cursor.
//! Actual errors retain their failed latch, budgets and pinned identity.
use super::tests::{Reply, deadline, payload, reader, start_server};
use super::*;
use std::time::Duration;

#[tokio::test]
async fn interrupted_body_keeps_its_exact_segment_without_another_get() {
    let server = start_server(Reply::HoldTail, 512, false).await;
    let mut reader = reader(&server, Limits::default()).await;
    reader.read_exact_at(0, 2, deadline()).await.unwrap();
    // HoldTail also holds control tails; this two-byte read has no tail.
    let mut stream = reader
        .into_stream_at(7, 16, StreamLimits::default())
        .unwrap();
    let first = stream.chunk().await.unwrap().unwrap();
    assert_eq!(&first[..], payload(7, 3));
    server.arrived.notified().await;
    let position = stream.position();
    for _ in 0..3 {
        {
            let wait = stream.chunk();
            tokio::pin!(wait);
            assert!(
                tokio::time::timeout(Duration::from_millis(5), &mut wait)
                    .await
                    .is_err()
            );
        }
        assert_eq!(stream.chunk().await, Err(Error::PreviouslyFailed));
        stream.resume_after_interruption().unwrap();
        assert_eq!(stream.position(), position);
        assert_eq!(stream.bytes_received(), 3);
        assert_eq!(stream.requests(), 1);
    }
    server.release.notify_one();
    let mut bytes = first.to_vec();
    loop {
        let chunk = tokio::select! {
            biased;
            _ = server.arrived.notified() => panic!("resuming the validated body issued another GET"),
            result = tokio::time::timeout(Duration::from_secs(1), stream.chunk()) => result.unwrap().unwrap(),
        };
        let Some(chunk) = chunk else { break };
        bytes.extend_from_slice(&chunk);
    }
    assert_eq!(bytes, payload(7, 16));
    assert_eq!(stream.position(), 23);
    assert_eq!(stream.bytes_received(), 16);
    assert_eq!(stream.requests(), 1);
    assert_eq!(stream.reader().requests(), 1);
    assert_eq!(stream.reader().bytes_received(), 2);
    assert_eq!(stream.object_length(), Some(512));
    assert_eq!(server.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn interrupted_headers_retry_from_the_same_position_with_the_same_budget() {
    let server = start_server(Reply::HoldHeaders, 512, false).await;
    let reader = reader(&server, Limits::default()).await;
    let mut stream = reader
        .into_stream_at(
            7,
            16,
            StreamLimits {
                max_requests: 2,
                ..StreamLimits::default()
            },
        )
        .unwrap();
    {
        let wait = stream.chunk();
        tokio::pin!(wait);
        tokio::select! { r = &mut wait => panic!("headers completed: {r:?}"), _ = server.arrived.notified() => {} }
    }
    assert_eq!(stream.requests(), 1);
    stream.resume_after_interruption().unwrap();
    assert_eq!(stream.position(), 7);
    assert_eq!(stream.bytes_received(), 0);
    let first = {
        let wait = stream.chunk();
        tokio::pin!(wait);
        tokio::select! { r = &mut wait => panic!("headers completed: {r:?}"), _ = server.arrived.notified() => {} }
        server.release.notify_waiters();
        tokio::time::timeout(Duration::from_secs(1), wait)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    };
    let mut bytes = first.to_vec();
    while let Some(chunk) = stream.chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    assert_eq!(bytes, payload(7, 16));
    assert_eq!(stream.requests(), 2);
    assert_eq!(stream.reader().requests(), 0);
    assert_eq!(server.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn interruptions_do_not_reset_the_request_limit_or_revive_limit_failure() {
    let server = start_server(Reply::HangHeaders, 512, false).await;
    let reader = reader(&server, Limits::default()).await;
    let mut stream = reader
        .into_stream_at(
            7,
            16,
            StreamLimits {
                max_requests: 1,
                ..StreamLimits::default()
            },
        )
        .unwrap();
    {
        let wait = stream.chunk();
        tokio::pin!(wait);
        tokio::select! { r = &mut wait => panic!("headers completed: {r:?}"), _ = server.arrived.notified() => {} }
    }
    stream.resume_after_interruption().unwrap();
    assert_eq!(stream.requests(), 1);
    assert_eq!(stream.chunk().await, Err(Error::Limit));
    assert_eq!(
        stream.resume_after_interruption(),
        Err(Error::PreviouslyFailed)
    );
    assert_eq!(stream.chunk().await, Err(Error::PreviouslyFailed));
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn returned_transport_identity_extent_and_timeout_errors_stay_terminal() {
    for reply in [
        Reply::WrongStart,
        Reply::WrongEnd,
        Reply::ChangedTag,
        Reply::ChangedLength,
        Reply::Ignored,
        Reply::Gzip,
        Reply::Multipart,
        Reply::DuplicateRange,
        Reply::MissingRange,
        Reply::BodyShort,
        Reply::BodyLong,
        Reply::HangHeaders,
        Reply::HangBody,
    ] {
        let server = start_server(reply, 512, false).await;
        let reader = reader(&server, Limits::default()).await;
        let mut stream = reader
            .into_stream_at(
                7,
                16,
                StreamLimits {
                    header_timeout: Duration::from_millis(30),
                    idle_timeout: Duration::from_millis(30),
                    ..StreamLimits::default()
                },
            )
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if stream.chunk().await?.is_none() {
                    return Ok::<_, Error>(());
                }
            }
        })
        .await
        .unwrap();
        assert!(result.is_err(), "{reply:?}");
        assert_eq!(
            stream.resume_after_interruption(),
            Err(Error::PreviouslyFailed),
            "{reply:?}"
        );
        assert_eq!(stream.chunk().await, Err(Error::PreviouslyFailed));
        assert!(stream.reader().is_failed());
        assert_eq!(server.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn unpolled_idle_and_finished_reads_do_not_create_recovery_work() {
    let server = start_server(Reply::Fixed, 512, false).await;
    let mut reader = reader(&server, Limits::default()).await;
    let mut stream = reader.stream_at(7, 16, StreamLimits::default()).unwrap();
    drop(stream.chunk());
    stream.resume_after_interruption().unwrap();
    assert_eq!(stream.requests(), 0);
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    assert_eq!(bytes, payload(7, 16));
    for _ in 0..3 {
        stream.resume_after_interruption().unwrap();
        assert_eq!(stream.chunk().await.unwrap(), None);
    }
    assert_eq!(stream.requests(), 1);
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn moving_an_interrupted_stream_to_control_keeps_the_reader_failed() {
    let server = start_server(Reply::HangBody, 512, false).await;
    let reader = reader(&server, Limits::default()).await;
    let mut stream = reader
        .into_stream_at(7, 16, StreamLimits::default())
        .unwrap();
    {
        let wait = stream.chunk();
        tokio::pin!(wait);
        tokio::select! { r = &mut wait => panic!("body completed: {r:?}"), _ = server.arrived.notified() => {} }
    }
    // No explicit recovery: into_reader must not quietly erase the latch.
    let mut reader = stream.into_reader();
    assert!(reader.is_failed());
    assert_eq!(
        reader.read_exact_at(7, 1, deadline()).await,
        Err(Error::PreviouslyFailed)
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}
