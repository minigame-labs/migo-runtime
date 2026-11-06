use super::*;
use std::{collections::VecDeque, time::Duration};
struct Body {
    length: Option<u64>,
    chunks: VecDeque<Vec<u8>>,
    failed: bool,
}
impl AudioSpoolBody for Body {
    fn length(&self) -> Option<u64> {
        self.length
    }
    fn chunk(&mut self) -> AudioSpoolFuture<'_, Option<shared::audio_file::AudioFileChunk>> {
        Box::pin(async move {
            if self.failed {
                return Err(error());
            }
            Ok(self
                .chunks
                .pop_front()
                .map(shared::audio_file::AudioFileChunk::new))
        })
    }
}
fn body(data: Vec<u8>, length: Option<u64>) -> Box<dyn AudioSpoolBody> {
    Box::new(Body {
        length,
        chunks: data.chunks(65536).map(<[u8]>::to_vec).collect(),
        failed: false,
    })
}
fn fixture(name: &str) -> (std::path::PathBuf, Arc<Spool>) {
    let root = std::env::temp_dir().join(format!(
        "{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let context = LocalAudioFile {
        scheduler: Arc::new(migo_io::scheduler::IoScheduler::new(1081)),
        spool_dir: root.join("private-spool"),
        mounts: None,
        hooks: Default::default(),
    };
    let spool = Arc::new(Spool {
        context,
        budget: SpoolBudget::new(262144, 1).unwrap(),
        maximum: 131072,
    });
    (root, spool)
}
#[tokio::test]
async fn sealed_http_spool_preserves_all_bytes_and_existing_reader_capacity_credit() {
    let (root, spool) = fixture("migo_http_sealed");
    let data = (0..131071).map(|n| (n * 37) as u8).collect::<Vec<_>>();
    for length in [Some(data.len() as u64), None] {
        let source = spool
            .clone()
            .prepare(
                body(data.clone(), length),
                Arc::new(AtomicBool::new(false)),
                std::time::Instant::now() + Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(source.prefix(), &data[..8]);
        assert_eq!(source.length(), data.len() as u64);
        assert_eq!(source.queued_bytes(), 131072);
        let mut out = Vec::new();
        let mut offset = 0;
        while offset < data.len() {
            let n = (data.len() - offset).min(65536);
            let chunk = source
                .clone()
                .read_at(
                    offset as u64,
                    n,
                    Arc::new(AtomicBool::new(false)),
                    std::time::Instant::now() + Duration::from_secs(5),
                )
                .unwrap()
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            out.extend_from_slice(chunk.as_ref());
            offset += n;
        }
        assert_eq!(out, data, "HTTP spool changed complete original bytes");
        assert!(
            SpoolWriter::create(&root, &spool.budget, 1, Some(0)).is_err(),
            "sealed source returned its original disk slot early"
        );
        drop(source);
        assert!(SpoolWriter::create(&root, &spool.budget, 1, Some(0)).is_ok());
    }
    let _ = std::fs::remove_dir_all(root);
}
#[tokio::test]
async fn over_limit_short_failed_cancelled_or_expired_spools_never_expose_a_file() {
    let (root, spool) = fixture("migo_http_refused");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    assert!(
        spool
            .clone()
            .prepare(
                body(vec![1], Some(131073)),
                Arc::new(AtomicBool::new(false)),
                deadline
            )
            .await
            .is_err(),
        "HTTP length limit ignored before creating spool"
    );
    assert!(
        !spool.context.spool_dir.exists(),
        "oversize HTTP length reached filesystem creation"
    );
    for (data, length) in [(vec![1; 100], Some(101)), (vec![1; 131073], None)] {
        assert!(
            spool
                .clone()
                .prepare(
                    body(data, length),
                    Arc::new(AtomicBool::new(false)),
                    deadline
                )
                .await
                .is_err(),
            "partial HTTP spool was sealed successfully"
        );
    }
    assert!(
        spool
            .clone()
            .prepare(
                Box::new(Body {
                    length: None,
                    chunks: VecDeque::new(),
                    failed: true
                }),
                Arc::new(AtomicBool::new(false)),
                deadline
            )
            .await
            .is_err()
    );
    assert!(
        spool
            .clone()
            .prepare(
                body(vec![1], Some(1)),
                Arc::new(AtomicBool::new(true)),
                deadline
            )
            .await
            .is_err(),
        "cancelled HTTP preparation created a source"
    );
    assert!(
        matches!(spool.clone().prepare(body(vec![1],Some(1)),Arc::new(AtomicBool::new(false)),std::time::Instant::now()-Duration::from_secs(1)).await,Err(e) if e.code==ErrorCode::Timeout)
    );
    assert!(
        SpoolWriter::create(&root, &spool.budget, 1, Some(0)).is_ok(),
        "refused HTTP spool leaked disk credit"
    );
    let _ = std::fs::remove_dir_all(root);
}
#[tokio::test]
async fn original_session_close_interrupts_a_body_wait_without_publishing_sealed_storage() {
    struct Stalled;
    impl AudioSpoolBody for Stalled {
        fn length(&self) -> Option<u64> {
            None
        }
        fn chunk(&mut self) -> AudioSpoolFuture<'_, Option<shared::audio_file::AudioFileChunk>> {
            Box::pin(std::future::pending())
        }
    }
    let (root, spool) = fixture("migo_http_closed");
    let task = tokio::spawn(spool.clone().prepare(
        Box::new(Stalled),
        Arc::new(AtomicBool::new(false)),
        std::time::Instant::now() + Duration::from_secs(5),
    ));
    tokio::time::sleep(Duration::from_millis(20)).await;
    spool.context.scheduler.close();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err(),
        "closed session kept a spool body wait alive"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn sealed_http_media_slots_use_existing_admission_and_retain_both_slot_credits() {
    let (root, mut spool) = fixture("migo_http_media_credit");
    Arc::get_mut(&mut spool).unwrap().context.scheduler =
        Arc::new(migo_io::scheduler::IoScheduler::isolated_for_admission_probe(1085, 1, 131071));
    assert!(
        spool
            .clone()
            .prepare(
                body(vec![7; 17], Some(17)),
                Arc::new(AtomicBool::new(false)),
                std::time::Instant::now() + Duration::from_secs(5)
            )
            .await
            .is_err(),
        "HTTP sealed media slots bypassed existing IO byte admission"
    );
    Arc::get_mut(&mut spool).unwrap().context.scheduler =
        Arc::new(migo_io::scheduler::IoScheduler::isolated_for_admission_probe(1086, 1, 131072));
    let source = spool
        .clone()
        .prepare(
            body(vec![7; 17], Some(17)),
            Arc::new(AtomicBool::new(false)),
            std::time::Instant::now() + Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert!(
        spool.context.scheduler.pools().reserve_bytes(1).is_err(),
        "HTTP sealed reader refunded its two-slot storage credit early"
    );
    drop(source);
    assert!(
        spool
            .context
            .scheduler
            .pools()
            .reserve_bytes(131072)
            .is_ok()
    );
    let _ = std::fs::remove_dir_all(root);
}
