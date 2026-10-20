//! getShowSplashAdStatus: the host's own cover advert, as the host last reported
//! it -- the platform's initial unknown / -1 before any report.

use deno_core::FastString;

use super::support::{SETTLE, boot_with_services, run_to_idle as run};

fn report(json: &str) -> String {
    format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('_internalOnAdEvent', \
         JSON.stringify([JSON.stringify({json})]));"
    )
}

#[test]
fn the_status_is_the_hosts_last_report_and_unknown_before_one() {
    let mut runtime = boot_with_services(None, None);
    runtime
        .execute_script("<test:splash-setup>", FastString::from_static(SETTLE))
        .unwrap();
    let before = run(
        &mut runtime,
        "__settle('getShowSplashAdStatus');",
        "JSON.stringify(__results.getShowSplashAdStatus)",
    );
    assert_eq!(
        before,
        r#"{"ok":true,"res":{"errMsg":"getShowSplashAdStatus:ok","status":"unknown","code":-1}}"#
    );

    let script = report(r#"{ event: 'splashStatus', status: 'success', code: 1 }"#)
        // One the platform does not define changes nothing.
        + &report(r#"{ event: 'splashStatus', status: 'shown', code: 9 }"#)
        + "__settle('getShowSplashAdStatus');";
    let after = run(
        &mut runtime,
        &script,
        "JSON.stringify([__results.getShowSplashAdStatus, typeof migo.getShowSplashAdStatus().then])",
    );
    assert_eq!(
        after,
        r#"[{"ok":true,"res":{"errMsg":"getShowSplashAdStatus:ok","status":"success","code":1}},"function"]"#,
        "and without callbacks, a Promise, as every asynchronous call returns"
    );
}
