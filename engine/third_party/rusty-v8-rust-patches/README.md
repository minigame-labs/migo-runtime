# Rusty V8 Rust consumer patch

The published V8 150.4.0 archive is pinned by `UPSTREAM`. `SOURCE_LAYOUT.json`
selects the complete Rust `src/` and generated FFI `gen/`, manifests, review
README and retained legal input; recorded patches yield the 119-file consumer in
`../v8-150.4.0`. Upstream GN/C++ dependencies, tools, examples and tests are not
part of this Cargo dependency. All omitted files are named by the verified
published inventory and ignored locally; they are never deleted by the gate.

`0003-component-consumer.patch` makes the build script std-only and removes six
native-build-only build dependencies. Normal builds require materialised local
archive and binding paths. The script links the verified component directly,
without another 130MiB archive copy or a shared target/gn_out directory. It
refuses native source mode and has no network downloader; native builds belong
to the separate source-locked platform scripts. Documentation and trybuild keep
their explicit non-linking/compile-only paths.

This changes repository/build inputs, not the runtime Rust API or the native
V8 implementation. Performance of the runtime still requires its own tests.
The complete published archive and native source locks remain available for
review and reproducing the omitted upstream sources.

`0001-windows-host-callbacks.patch` mirrors the Rust portion of the native
Windows callback patch. The DLL and the Rust consumer must agree on all 36
slots: the original serializer/cppgc/inspector callbacks, six CustomPlatform
callbacks, and six CRDTP callbacks. Registration occurs before constructing a
CustomPlatform, including its possible early destruction.

Patching only the separate native-build checkout does not patch the registry
crate compiled into Migo. The workspace therefore selects this verified path
crate. The contract gate reconstructs the checksum-verified published crate plus ordered patches,
checks the declared source projection and compares every retained file and checks the actual Cargo selection, with negative
controls for an unrecorded edit and a missing patch.

Native C++ builds continue to use the pinned standalone source checkout and
`../v8-patches/`; this Rust consumer patch does not stand in for native artifact
regeneration, Windows DLL linking, or execution tests.

0002-bounded-serializer.patch adds an optional Rust delegate allocation cap,
preserving the unlimited default and every C callback signature. Rejected or
failed reallocations preserve the old allocation's layout. Migo's core codec
uses the cap before allocating serialized Worker data; the Windows source gate
replays this common Rust-only patch after the native callback mirror.
