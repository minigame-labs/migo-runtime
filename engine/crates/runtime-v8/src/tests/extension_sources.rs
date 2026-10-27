//! Every source an extension declares travels inside the binary.
//!
//! deno_core 0.413 recorded an extension's `esm`/`js` files by their absolute path on the build machine and read
//! them from there whenever a runtime booted from source. On the build machine everything passed. On a phone the
//! first such runtime aborted with "Failed to initialize a JsRuntime: No such file or directory": every Worker (the
//! release ships no Worker snapshot), every build whose host snapshot was stale, and the snapshot generator itself,
//! which runs on the device it targets. `engine/third_party/deno-core-patches/0006-embedded-extension-sources.patch`
//! embeds them again; this holds it.

#[test]
fn every_declared_source_is_loadable_off_the_build_machine() {
    let extensions = crate::snapshot::lazy_extensions().into_iter();
    #[cfg(feature = "api-system")]
    let extensions = extensions.chain(crate::snapshot::worker_lazy_extensions());

    let mut sources = 0;
    for extension in extensions {
        for source in extension
            .esm_files
            .iter()
            .chain(extension.js_files.iter())
            .chain(extension.lazy_loaded_esm_files.iter())
            .chain(extension.lazy_loaded_js_files.iter())
        {
            assert!(
                source.is_runtime_loadable(),
                "{} of {} is read from the build machine's disk",
                source.specifier,
                extension.name
            );
            sources += 1;
        }
    }
    // host_v8_base declares its sources in every build: a walk that finds none is not looking.
    assert!(sources > 0, "no extension declared a source");
}
