use super::*;
use std::path::PathBuf;

/// A sandbox with one file in `/user`, which is what an upload reads from.
fn sandbox(name: &str) -> (PathBuf, shared::vfs::VirtualFS) {
    let base = std::env::temp_dir().join(format!("migo-upload-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (code, user, cache, tmp) = (
        base.join("code"),
        base.join("user"),
        base.join("cache"),
        base.join("tmp"),
    );
    for directory in [&code, &user, &cache, &tmp] {
        std::fs::create_dir_all(directory).unwrap();
    }
    std::fs::write(user.join("save.png"), b"not really a png").unwrap();
    (base, shared::vfs::VirtualFS::new(code, user, cache, tmp))
}

fn env<'a>(
    policy: &'a NetworkPolicy,
    client: &'a Client,
    resources: &'a ResourceTable,
    vfs: Option<&'a shared::vfs::VirtualFS>,
) -> UploadEnv<'a> {
    UploadEnv {
        policy,
        client,
        resources,
        vfs,
        mount_table: None,
    }
}

fn request(cancel_rid: ResourceId, url: &str, file_path: &str) -> UploadRequest {
    UploadRequest {
        cancel_rid,
        url: url.to_string(),
        file_path: file_path.to_string(),
        name: "file".to_string(),
        filename: "save.png".to_string(),
        headers: Vec::new(),
        form_data: Vec::new(),
        timeout_ms: 30_000,
    }
}

fn client() -> Client {
    super::super::client::create_policy_http_client(
        "migo",
        false,
        &NetworkPolicy::default(),
        GateKind::FetchRedirect,
        "fetch",
    )
    .expect("a client")
}

/// The media type is the one the file's name implies, and an unknown or absent
/// extension is bytes.
#[test]
fn a_file_s_name_decides_what_the_server_is_told_it_is() {
    assert_eq!(mime_of("shot.PNG"), "image/png");
    assert_eq!(mime_of("clip.jpeg"), "image/jpeg");
    assert_eq!(mime_of("save.dat"), "application/octet-stream");
    assert_eq!(mime_of("noextension"), "application/octet-stream");
}

/// A handle content has already closed means the upload was aborted, which is
/// answered rather than thrown -- and nothing is opened or sent for it.
#[tokio::test]
async fn an_upload_whose_handle_is_gone_is_answered_as_aborted() {
    let resources = ResourceTable::new();
    let rid = cancel_handle(&resources);
    resources.close(rid);
    let policy = NetworkPolicy::default();
    let client = client();
    let (base, vfs) = sandbox("aborted");
    let answer = upload(
        env(&policy, &client, &resources, Some(&vfs)),
        request(rid, "https://allowed.example/upload", "/user/save.png"),
    )
    .await
    .expect("an abort is an answer, not a throw");
    assert_eq!(answer, UploadAnswer::aborted());
    assert_eq!(answer.error.as_deref(), Some("uploadFile:fail aborted"));
    let _ = std::fs::remove_dir_all(base);
}

/// A URL that is not one is refused after the file is opened and before
/// anything is sent -- the same TypeError `fetch` gives.
#[tokio::test]
async fn a_url_that_is_not_one_is_a_type_error() {
    let resources = ResourceTable::new();
    let policy = NetworkPolicy::default();
    let client = client();
    let (base, vfs) = sandbox("badurl");
    let error = upload(
        env(&policy, &client, &resources, Some(&vfs)),
        request(0, "not a url", "/user/save.png"),
    )
    .await
    .expect_err("a URL is a URL");
    assert_eq!(error.class, "TypeError");
    assert_eq!(error.message, "Invalid URL");
    let _ = std::fs::remove_dir_all(base);
}

/// The destination is held to the policy, and a host it refuses is never
/// connected to -- after the file was read, because the file is content's own.
#[tokio::test]
async fn a_host_the_policy_refuses_is_never_uploaded_to() {
    let resources = ResourceTable::new();
    let policy = NetworkPolicy {
        domain_whitelist: vec!["allowed.example".to_string()],
        enforce_https: true,
    };
    let client = client();
    let (base, vfs) = sandbox("blocked");
    let error = upload(
        env(&policy, &client, &resources, Some(&vfs)),
        request(0, "https://blocked.example/upload", "/user/save.png"),
    )
    .await
    .expect_err("the allow list refuses it");
    assert!(
        error.message.contains("blocked.example"),
        "{}",
        error.message
    );
    let _ = std::fs::remove_dir_all(base);
}

/// A path outside the sandbox is refused with the words the facade reports,
/// naming the path content gave.
#[tokio::test]
async fn a_path_outside_the_sandbox_is_refused() {
    let resources = ResourceTable::new();
    let policy = NetworkPolicy::default();
    let client = client();
    let (base, vfs) = sandbox("outside");
    let error = upload(
        env(&policy, &client, &resources, Some(&vfs)),
        request(0, "https://allowed.example/upload", "/etc/passwd"),
    )
    .await
    .expect_err("only the sandbox is readable");
    assert!(
        error.message.starts_with("uploadFile:fail path not allowed: /etc/passwd"),
        "{}",
        error.message
    );
    let _ = std::fs::remove_dir_all(base);
}

/// Without a sandbox there is nothing to read, and the refusal says so before
/// any network work.
#[tokio::test]
async fn an_upload_with_no_sandbox_is_refused_by_path() {
    let resources = ResourceTable::new();
    let policy = NetworkPolicy::default();
    let client = client();
    let error = upload(
        env(&policy, &client, &resources, None),
        request(0, "https://allowed.example/upload", "/user/save.png"),
    )
    .await
    .expect_err("there is no VFS to resolve against");
    assert_eq!(error.message, "uploadFile:fail VFS not initialised");
}

/// A file inside a package cannot be streamed, and the refusal names the path
/// content gave rather than the engine's own.
#[test]
fn a_packaged_file_cannot_be_streamed() {
    let error = resolve_upload_path(None, None, "/user/save.png")
        .expect_err("no VFS, no path");
    assert_eq!(error.message, "uploadFile:fail VFS not initialised");
}

/// The cancel handle is a resource of the session, so closing it is what
/// aborts -- and it is a fetch cancel handle, the same kind a download's is.
#[test]
fn the_handle_an_upload_is_aborted_with_is_the_session_s() {
    let resources = ResourceTable::new();
    let rid = cancel_handle(&resources);
    let flag = resources.cancel_flag(rid).expect("a cancel flag");
    assert!(!flag.is_cancelled());
    resources.close(rid);
    assert!(flag.is_cancelled(), "abort() is a close of this handle");
    assert!(
        resources.cancel_flag(rid).is_none(),
        "a closed handle names nothing"
    );
}
