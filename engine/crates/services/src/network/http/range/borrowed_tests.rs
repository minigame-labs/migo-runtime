use super::tests::reader as source_reader;
use super::{tests::*, *};
use std::time::Duration;

#[tokio::test]
async fn borrowed_reads_fill_exact_windows_and_share_counters_with_vec_reads() {
    for (reply, unknown) in [
        (Reply::Fixed, false),
        (Reply::Chunked, false),
        (Reply::Short, false),
        (Reply::UnknownTotal, true),
    ] {
        let server = start_server(reply, 5_000_000_000, unknown).await;
        let mut reader = source_reader(&server, Limits::default()).await;
        let offset = 4_294_967_319;
        let mut output = Vec::with_capacity(4096);
        output.resize(105, 0xa5);
        let pointer = output.as_ptr();
        let capacity = output.capacity();
        reader
            .read_exact_into(offset, &mut output[16..89], deadline())
            .await
            .expect("borrowed control rejected a valid absolute source window");
        assert_eq!(
            &output[16..89],
            payload(offset, 73),
            "borrowed range did not fill the complete target"
        );
        assert!(
            output[..16].iter().chain(&output[89..]).all(|b| *b == 0xa5),
            "borrowed control wrote beyond its destination"
        );
        assert_eq!(output.as_ptr(), pointer);
        assert_eq!(output.capacity(), capacity);
        assert_eq!(
            reader.bytes_received(),
            73,
            "borrowed reads reset shared byte credit"
        );
        let requests = reader.requests();
        assert_eq!(
            reader
                .read_exact_at(offset + 73, 17, deadline())
                .await
                .unwrap(),
            payload(offset + 73, 17)
        );
        assert_eq!(
            reader.bytes_received(),
            90,
            "Vec and borrowed controls lost their common byte budget"
        );
        assert!(reader.requests() > requests);
        assert!(!reader.is_failed());
        assert_eq!(
            server.requests.lock().unwrap().len(),
            reader.requests() as usize + 1
        );
    }
}

#[tokio::test]
async fn borrowed_refusals_preserve_the_destination_and_do_not_issue_a_get() {
    for (offset, len, expired) in [
        (16, 0, false),
        (16, 33, false),
        (500, 16, false),
        (u64::MAX - 7, 16, false),
        (16, 16, true),
    ] {
        let server = start_server(Reply::Fixed, 512, false).await;
        let mut reader = source_reader(
            &server,
            Limits {
                max_read_bytes: 32,
                ..Limits::default()
            },
        )
        .await;
        let mut output = [0xa5; 64];
        let until = if expired {
            Instant::now() - Duration::from_millis(1)
        } else {
            deadline()
        };
        let result = reader
            .read_exact_into(offset, &mut output[..len], until)
            .await;
        assert_eq!(
            result,
            Err(if expired {
                Error::Timeout
            } else {
                Error::Limit
            }),
            "borrowed control accepted an invalid extent or expired deadline"
        );
        assert_eq!(
            output, [0xa5; 64],
            "borrowed control refusal changed destination"
        );
        assert!(reader.is_failed());
        assert_eq!(
            reader
                .read_exact_into(0, &mut output[..8], deadline())
                .await,
            Err(Error::PreviouslyFailed)
        );
        assert_eq!(
            server.requests.lock().unwrap().len(),
            1,
            "refused borrowed control issued network IO"
        );
    }
}

#[tokio::test]
async fn borrowed_reads_reject_changed_identity_and_malformed_or_truncated_bodies() {
    for reply in [
        Reply::WrongStart,
        Reply::WrongEnd,
        Reply::ChangedTag,
        Reply::MissingTag,
        Reply::ChangedLength,
        Reply::Ignored,
        Reply::ChangedFull,
        Reply::Gzip,
        Reply::OtherCoding,
        Reply::Multipart,
        Reply::DuplicateRange,
        Reply::MissingRange,
        Reply::BodyShort,
        Reply::BodyLong,
        Reply::Redirect,
    ] {
        let server = start_server(reply, 512, false).await;
        let mut reader = source_reader(&server, Limits::default()).await;
        let mut output = [0xa5; 48];
        assert!(
            reader
                .read_exact_into(32, &mut output[8..40], deadline())
                .await
                .is_err(),
            "borrowed control accepted an unpinned or incomplete representation: {reply:?}"
        );
        assert!(output[..8].iter().chain(&output[40..]).all(|b| *b == 0xa5));
        assert!(reader.is_failed());
        let requests = server.requests.lock().unwrap().len();
        assert_eq!(
            reader
                .read_exact_into(32, &mut output[8..40], deadline())
                .await,
            Err(Error::PreviouslyFailed)
        );
        assert_eq!(server.requests.lock().unwrap().len(), requests);
    }
}

#[tokio::test]
async fn borrowed_short_responses_keep_one_absolute_deadline() {
    let server = start_server(Reply::SlowShort, 512, false).await;
    let mut reader = source_reader(&server, Limits::default()).await;
    let mut output = [0xa5; 128];
    let start = Instant::now();
    assert_eq!(
        reader
            .read_exact_into(32, &mut output, start + Duration::from_millis(80))
            .await,
        Err(Error::Timeout),
        "borrowed control renewed the original deadline"
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(reader.is_failed());
}

#[tokio::test]
async fn dropped_borrowed_wait_retires_the_same_reader() {
    let server = start_server(Reply::HoldHeaders, 512, false).await;
    let mut reader = source_reader(&server, Limits::default()).await;
    let mut output = [0xa5; 64];
    {
        let operation = reader.read_exact_into(32, &mut output, deadline());
        tokio::pin!(operation);
        tokio::select! {
            _=server.arrived.notified()=>{},
            result=&mut operation=>panic!("blocked borrowed read unexpectedly completed: {result:?}"),
        }
    }
    assert!(
        reader.is_failed(),
        "dropped borrowed control future left its reader reusable"
    );
    assert_eq!(output, [0xa5; 64]);
    assert_eq!(
        reader.read_exact_into(32, &mut output, deadline()).await,
        Err(Error::PreviouslyFailed)
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    server.release.notify_waiters();
}

#[tokio::test]
async fn vec_and_borrowed_reads_cannot_mint_byte_or_request_credit() {
    let server = start_server(Reply::Fixed, 512, false).await;
    let mut reader = source_reader(
        &server,
        Limits {
            max_total_bytes: 12,
            ..Limits::default()
        },
    )
    .await;
    reader.read_exact_at(16, 8, deadline()).await.unwrap();
    let requests = server.requests.lock().unwrap().len();
    let mut output = [0xa5; 8];
    assert_eq!(
        reader.read_exact_into(32, &mut output, deadline()).await,
        Err(Error::Limit)
    );
    assert_eq!(
        server.requests.lock().unwrap().len(),
        requests,
        "borrowed control preflight budget allowed network IO"
    );
    assert_eq!(
        output, [0xa5; 8],
        "borrowed control budget refusal changed destination"
    );
    assert_eq!(reader.bytes_received(), 8);
    let server = start_server(Reply::Short, 512, false).await;
    let mut reader = source_reader(
        &server,
        Limits {
            max_requests: 3,
            ..Limits::default()
        },
    )
    .await;
    reader.read_exact_at(16, 4, deadline()).await.unwrap();
    let mut output = [0xa5; 12];
    assert_eq!(
        reader.read_exact_into(32, &mut output, deadline()).await,
        Err(Error::Limit),
        "borrowed reads minted fresh request credit"
    );
    assert_eq!(&output[..8], payload(32, 8));
    assert_eq!(&output[8..], [0xa5; 4]);
    assert_eq!(reader.requests(), 3);
    assert_eq!(reader.bytes_received(), 12);
}
