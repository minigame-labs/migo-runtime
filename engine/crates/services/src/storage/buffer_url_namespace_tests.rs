use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "migo-url-namespace-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn vfs(&self) -> VirtualFS {
        VirtualFS::new(
            self.0.join("code"),
            self.0.join("user"),
            self.0.clone(),
            self.0.join("tmp"),
        )
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn revocation_accepts_only_one_child_of_the_virtual_buffer_directory() {
    let root = Scratch::new();
    let vfs = root.vfs();
    let url = create_buffer_url(Some(&vfs), b"issued").unwrap();
    assert!(url.starts_with("/cache/buffer_urls/buffer-"));
    assert!(!url.contains(root.0.to_str().unwrap()));
    fs::write(root.0.join("outside"), b"outside").unwrap();
    let buffers = root.0.join(BUFFER_URL_DIR);
    fs::write(buffers.join("victim"), b"keep").unwrap();
    fs::create_dir(buffers.join("nested")).unwrap();
    for suffix in [
        "../outside",
        "nested/../victim",
        "./victim",
        "victim/",
        "/victim",
        "victim:stream",
        "..\\outside",
    ] {
        revoke_buffer_url(Some(&vfs), &format!("{BUFFER_URL_ROOT}/{suffix}")).unwrap();
    }
    revoke_buffer_url(Some(&vfs), "/cache/buffer_urls-extra/../buffer_urls/victim").unwrap();
    revoke_buffer_url(Some(&vfs), &buffers.join("victim").to_string_lossy()).unwrap();
    assert_eq!(fs::read(root.0.join("outside")).unwrap(), b"outside");
    assert_eq!(fs::read(buffers.join("victim")).unwrap(), b"keep");
    revoke_buffer_url(Some(&vfs), &url).unwrap();
    revoke_buffer_url(Some(&vfs), &url).unwrap();
    assert!(!root.0.join(url.strip_prefix("/cache/").unwrap()).exists());
}

#[cfg(unix)]
#[test]
fn buffer_services_refuse_a_replaced_directory_symlink() {
    let root = Scratch::new();
    let vfs = root.vfs();
    let url = create_buffer_url(Some(&vfs), b"issued").unwrap();
    let name = url.strip_prefix(&format!("{BUFFER_URL_ROOT}/")).unwrap();
    let outside = root.0.join("outside-dir");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join(name), b"outside").unwrap();
    fs::rename(root.0.join(BUFFER_URL_DIR), root.0.join("old-buffers")).unwrap();
    std::os::unix::fs::symlink(&outside, root.0.join(BUFFER_URL_DIR)).unwrap();
    assert!(create_buffer_url(Some(&vfs), b"must not escape").is_err());
    revoke_buffer_url(Some(&vfs), &url).unwrap();
    assert_eq!(fs::read(outside.join(name)).unwrap(), b"outside");
    assert_eq!(fs::read_dir(outside).unwrap().count(), 1);
    assert_eq!(
        fs::read(root.0.join("old-buffers").join(name)).unwrap(),
        b"issued"
    );
}

#[cfg(unix)]
#[test]
fn public_buffer_urls_use_the_captured_cache_mapping_after_root_replacement() {
    let root = Scratch::new();
    let cache = root.0.join("cache");
    fs::create_dir(&cache).unwrap();
    let vfs = VirtualFS::new(
        root.0.join("code"),
        root.0.join("user"),
        cache.clone(),
        root.0.join("tmp"),
    );
    fs::rename(&cache, root.0.join("original-cache")).unwrap();
    fs::create_dir(&cache).unwrap();
    let url = create_buffer_url(Some(&vfs), b"pinned").unwrap();
    let relative = url.strip_prefix("/cache/").unwrap();
    assert_eq!(
        fs::read(root.0.join("original-cache").join(relative)).unwrap(),
        b"pinned"
    );
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 0);
    revoke_buffer_url(Some(&vfs), &url).unwrap();
    assert!(!root.0.join("original-cache").join(relative).exists());
}

#[test]
fn revocation_is_lazy_and_unloaded_services_fail_before_filesystem_access() {
    let root = Scratch::new();
    assert!(create_buffer_url(None, b"unloaded").is_err());
    assert!(revoke_buffer_url(None, "/cache/buffer_urls/none").is_err());
    let vfs = root.vfs();
    revoke_buffer_url(Some(&vfs), "/cache/buffer_urls/none").unwrap();
    assert!(!root.0.join(BUFFER_URL_DIR).exists());
}
