use super::*;
use shared::vfs::file_mutation::FileMutations;
use std::{collections::VecDeque, fs::File, sync::Barrier, thread};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let id = NEXT_BUFFER_URL.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("migo-buffer-url-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn exclusive_creation_skips_existing_objects_without_truncating_or_following_them() {
    let root = Scratch::new();
    let occupied = root.0.join("occupied");
    fs::write(&occupied, b"another object").unwrap();
    let held = File::open(&occupied).unwrap();
    let mutations = FileMutations::for_file(&held).unwrap().unwrap();
    let before = mutations.read_epoch().unwrap();
    let mut names = VecDeque::from(["occupied", "new"]);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&occupied, root.0.join("link")).unwrap();
        names.insert(1, "link");
    }
    let path = create_buffer_url_in(
        &root.0,
        || Ok(names.pop_front().unwrap().to_owned()),
        |file| file.write_all(b"new payload"),
    )
    .unwrap();
    assert_eq!(path, root.0.join("new"));
    assert_eq!(fs::read(path).unwrap(), b"new payload");
    assert_eq!(fs::read(occupied).unwrap(), b"another object");
    assert!(before.verify().is_ok());
}

#[test]
fn buffer_write_tracks_the_actual_open_object_and_releases_its_epoch() {
    let root = Scratch::new();
    let mut held = None;
    let path = create_buffer_url_in(
        &root.0,
        || Ok("new".to_owned()),
        |file| {
            let reader = File::open(root.0.join("new"))?;
            let mutations = FileMutations::for_file(&reader)?.unwrap();
            assert!(mutations.read_epoch().is_err(), "buffer write is untracked");
            file.write_all(b"first")?;
            assert!(
                mutations.read_epoch().is_err(),
                "epoch ended between writes"
            );
            file.write_all(b" second")?;
            held = Some((reader, mutations));
            Ok(())
        },
    )
    .unwrap();
    let (_reader, mutations) = held.unwrap();
    assert!(mutations.read_epoch().is_ok());
    assert_eq!(fs::read(path).unwrap(), b"first second");
}

#[test]
fn a_partial_write_error_removes_the_failed_url_and_releases_the_epoch() {
    let root = Scratch::new();
    let mut held = None;
    let error = create_buffer_url_in(
        &root.0,
        || Ok("failed".to_owned()),
        |file| {
            let reader = File::open(root.0.join("failed"))?;
            let mutations = FileMutations::for_file(&reader)?.unwrap();
            assert!(mutations.read_epoch().is_err());
            file.write_all(b"partial")?;
            held = Some((reader, mutations));
            Err(io::Error::other("injected write failure"))
        },
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "injected write failure");
    assert!(
        !root.0.join("failed").exists(),
        "failed URL was left visible"
    );
    let (_reader, mutations) = held.unwrap();
    assert!(mutations.read_epoch().is_ok());
}

#[test]
fn concurrent_public_buffer_urls_keep_complete_independent_payloads() {
    let root = Scratch::new();
    let paths =
        Arc::new(GamePaths::new(root.0.join("files"), root.0.join("cache"), "game", 7).unwrap());
    let barrier = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8_u8)
        .map(|byte| {
            let paths = paths.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let payload = vec![byte; 128 * 1024 + byte as usize];
                barrier.wait();
                let path = create_buffer_url(Some(&paths), &payload).unwrap();
                assert_eq!(fs::read(&path).unwrap(), payload);
                (path, payload)
            })
        })
        .collect();
    let values: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    let names: std::collections::HashSet<_> = values.iter().map(|(path, _)| path).collect();
    assert_eq!(names.len(), values.len());
    for (path, payload) in values {
        assert_eq!(fs::read(&path).unwrap(), payload);
        revoke_buffer_url(Some(&paths), &path).unwrap();
        assert!(!Path::new(&path).exists());
    }
}
