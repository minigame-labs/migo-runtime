//! The native archive must use Migo's package, without a hidden full fallback.
use deno_core::{JsRuntime, RuntimeOptions};

fn run_intl(install: bool) -> Result<(), String> {
    if install {
        runtime_v8::install_icu_data();
    }
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _enter = executor.enter();
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    runtime
        .execute_script(
            "external-icu",
            "if (new Intl.NumberFormat('de-DE').format(1234.5) !== '1.234,5') throw new Error('ICU number data');\n\
             if (new Intl.DateTimeFormat('en-US', {year:'numeric',timeZone:'UTC'}).format(new Date(0)) !== '1970') throw new Error('ICU date data');\n\
             if (Array.from(new Intl.Segmenter('en', {granularity:'word'}).segment('mini game')).length !== 3) throw new Error('ICU segment data');",
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[test]
fn filtered_embedder_data_supports_intl_without_a_data_file() {
    run_intl(true).expect("the embedded filtered package must support Intl");
}

#[test]
fn native_archive_has_no_hidden_full_icu_fallback() {
    // A fresh process is essential: installing ICU is process-wide and cannot
    // be undone. The positive test in this same executable proves that the
    // binary, loader, Tokio driver and script are otherwise usable.
    let empty = std::env::temp_dir().join(format!(
        "migo-no-icu-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&empty).unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "without_embedder_data_child",
            "--ignored",
            "--nocapture",
        ])
        .env("MIGO_ICU_NO_INSTALL_CONTROL", "1")
        .env("ICU_DATA", &empty)
        .current_dir(&empty)
        .output()
        .expect("start independent ICU control");
    std::fs::remove_dir_all(empty).unwrap();
    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    let diagnosed_icu_failure = child.status.code() == Some(9)
        || (child.status.code().is_none()
            && (output.to_lowercase().contains("icu") || output.contains("U_SUCCESS")));
    assert!(
        diagnosed_icu_failure,
        "the no-install control must fail specifically on ICU; status={:?}\n{output}",
        child.status
    );
}

#[test]
#[ignore = "private child process for the no-ICU negative control"]
fn without_embedder_data_child() {
    assert_eq!(
        std::env::var("MIGO_ICU_NO_INSTALL_CONTROL").as_deref(),
        Ok("1")
    );
    match run_intl(false) {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("no-install control: {error}");
            if error.to_lowercase().contains("icu") {
                std::process::exit(9);
            }
            std::process::exit(12);
        }
    }
}
