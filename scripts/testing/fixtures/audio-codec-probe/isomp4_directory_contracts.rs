//! Directory mount capabilities use the same file source, CPU steps and player.
use super::*;
use shared::vfs::{DirSource, MountTable, mount::PinnedDirectoryRead};

struct Tree(std::path::PathBuf);
impl Tree {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let fixtures = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap());
        let path = fixtures.parent().unwrap().join(format!(
            "sandbox/cache/directory-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn pin(name: &str) -> PinnedDirectoryRead {
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap());
    let mt = MountTable::new(path.clone());
    assert!(mt.mount_overlay(
        "fixture".into(),
        "level".into(),
        Arc::new(DirSource::try_new(path).unwrap())
    ));
    mt.pin_directory_read(name, mt.generation()).unwrap()
}
async fn produce<const B: bool>(
    pin: PinnedDirectoryRead,
    target: u32,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    steps: Arc<AtomicUsize>,
) {
    let owner = IoScheduler::new(0);
    let io_thread = std::thread::current().id();
    let prepared =
        transport::while_active(&tx, &state, FileReader::open_directory(pin, &owner)).await;
    let reader = match prepared {
        None => return,
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            transport::send(&tx, &state, StreamMsg::Error(e.to_string())).await;
            return;
        }
    };
    let length = reader.len();
    let result = transport::run_source(
        owner.audio_cpu(),
        IndexedFile::from_reader(reader),
        Some(length),
        &tx,
        &state,
        move |length| Observed::<B> {
            decoder: Batched::with_object_len(target, length),
            steps,
            io_thread,
        },
    )
    .await;
    transport::publish_result(&tx, &state, result).await;
}
async fn receive<const B: bool>(pin: PinnedDirectoryRead, target: u32) -> Received {
    let state = StreamingState::new();
    let (tx, mut rx) = stream_channel();
    let task = tokio::spawn(produce::<B>(
        pin,
        target,
        tx,
        state.clone(),
        Arc::new(AtomicUsize::new(0)),
    ));
    let mut got = Received {
        pcm: Vec::new(),
        ready: 0,
        done: 0,
        errors: Vec::new(),
    };
    while let Some(message) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
    {
        match message {
            StreamMsg::Ready { sample_rate, .. } => {
                assert_eq!(sample_rate, target);
                got.ready += 1;
            }
            StreamMsg::Samples(pcm) => {
                assert_eq!((got.ready, got.done, got.errors.len()), (1, 0, 0));
                got.pcm.extend_from_slice(&pcm);
            }
            StreamMsg::Done => got.done += 1,
            StreamMsg::Error(e) => got.errors.push(e),
        }
    }
    task.await.unwrap();
    assert_eq!(
        state.download_complete.load(Ordering::Acquire),
        got.done == 1
    );
    got
}
#[tokio::test]
async fn directory_layouts_and_resampling_equal_whole_pcm() {
    let mut comparisons = 0;
    for (i, name) in [
        "0-front.m4a",
        "1-tail.m4a",
        "2-reorder.m4a",
        "2-reverse.m4a",
        "2-tail-multi.m4a",
        "1-metadata.m4a",
        "2-buffered-miss.m4a",
        "2-coalesced-rewind.m4a",
    ]
    .into_iter()
    .enumerate()
    {
        let data = fixture(name);
        let rate = candidate::decode(&data, 16_777_216).unwrap().sample_rate;
        let selected = if i % 2 == 0 {
            name.to_owned()
        } else {
            format!("level/{name}")
        };
        for target in [rate, if rate == 48000 { 44100 } else { 48000 }] {
            let (expected, _) = reference(&data, target);
            for cached in [false, true] {
                let got = if cached {
                    receive::<true>(pin(&selected), target).await
                } else {
                    receive::<false>(pin(&selected), target).await
                };
                assert_eq!(
                    (got.ready, got.done, got.errors.len()),
                    (1, 1, 0),
                    "{selected}/{target}/{cached}: {:?}",
                    got.errors
                );
                assert_eq!(got.pcm, expected);
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 32);
    println!("W09_M4A_DIRECTORY_PCM {comparisons} exact comparisons");
}
#[tokio::test]
async fn retired_directory_capability_decodes_original_generation() {
    let t = Tree::new();
    let code = t.0.join("code");
    std::fs::create_dir(&code).unwrap();
    std::fs::write(code.join("clip"), fixture("1-tail.m4a")).unwrap();
    let mt = MountTable::new(code.clone());
    std::fs::rename(&code, t.0.join("retired")).unwrap();
    std::fs::create_dir(&code).unwrap();
    std::fs::write(code.join("clip"), fixture("2-reorder.m4a")).unwrap();
    let generation = mt.generation();
    let old = mt.pin_directory_read("clip", generation).unwrap();
    mt.swap_base(Arc::new(DirSource::try_new(code).unwrap()));
    assert!(mt.pin_directory_read("clip", generation).is_err());
    let new = mt.pin_directory_read("clip", mt.generation()).unwrap();
    assert_ne!(old.source_identity(), new.source_identity());
    for (selected, name) in [(old, "1-tail.m4a"), (new, "2-reorder.m4a")] {
        let got = receive::<true>(selected, 44100).await;
        assert_eq!(
            (got.ready, got.done, got.errors.len()),
            (1, 1, 0),
            "{:?}",
            got.errors
        );
        assert_eq!(got.pcm, reference(&fixture(name), 44100).0);
    }
    println!("W09_M4A_DIRECTORY_RETIRED_PCM 2 exact comparisons");
}
#[tokio::test]
async fn directory_setup_and_media_use_fs_when_pack_is_full() {
    let selected = pin("1-tail.m4a");
    let owner = IoScheduler::new(0);
    let _release = super::package::block_pack(&owner).await;
    let mut reader = tokio::time::timeout(
        Duration::from_secs(1),
        FileReader::open_directory(selected, &owner),
    )
    .await
    .expect("directory setup was incorrectly queued in Pack")
    .unwrap();
    let chunk = tokio::time::timeout(Duration::from_secs(1), reader.chunk())
        .await
        .expect("directory media was incorrectly queued in Pack")
        .unwrap();
    assert!(chunk.is_some());
}
#[tokio::test]
async fn directory_player_preserves_pause_pcm_and_exact_ended() {
    for (name, cached) in [("2-reverse.m4a", false), ("2-tail-multi.m4a", true)] {
        for target in [48000, 44100] {
            let (expected, channels) = reference(&fixture(name), target);
            let frames = expected.len() / channels as usize;
            let state = StreamingState::new();
            let (tx, rx) = stream_channel();
            let steps = Arc::new(AtomicUsize::new(0));
            let selected = pin(&format!("level/{name}"));
            let task = if cached {
                tokio::spawn(produce::<true>(
                    selected,
                    target,
                    tx,
                    state.clone(),
                    steps.clone(),
                ))
            } else {
                tokio::spawn(produce::<false>(
                    selected,
                    target,
                    tx,
                    state.clone(),
                    steps.clone(),
                ))
            };
            if cached {
                consume_player::<true>(
                    name,
                    format!("directory://{name}"),
                    target,
                    true,
                    false,
                    expected,
                    channels,
                    frames,
                    state,
                    steps,
                    task,
                    rx,
                )
                .await;
            } else {
                consume_player::<false>(
                    name,
                    format!("directory://{name}"),
                    target,
                    true,
                    false,
                    expected,
                    channels,
                    frames,
                    state,
                    steps,
                    task,
                    rx,
                )
                .await;
            }
        }
    }
}
#[tokio::test]
async fn directory_admission_and_open_fail_closed_without_source_fallback() {
    let t = Tree::new();
    let mt = MountTable::new(t.0.clone());
    for name in ["/clip", "../clip", "NUL", "clip:stream", "clip\\escape"] {
        assert!(
            mt.pin_directory_read(name, mt.generation()).is_err(),
            "{name}"
        );
    }
    let owner = IoScheduler::new(0);
    let absent = mt.pin_directory_read("absent", mt.generation()).unwrap();
    assert!(FileReader::open_directory(absent, &owner).await.is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            std::env::var_os("W09_M4A_FIXTURES").unwrap(),
            t.0.join("outside"),
        )
        .unwrap();
        let linked = mt
            .pin_directory_read("outside/1-tail.m4a", mt.generation())
            .unwrap();
        assert!(FileReader::open_directory(linked, &owner).await.is_err());
    }
    let selected = pin("1-tail.m4a");
    owner.domain().close_all();
    assert!(FileReader::open_directory(selected, &owner).await.is_err());
}
