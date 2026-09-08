# MigoMacV8

Lane 3, macOS only: in-process V8 with JIT, direct bindings, the existing
`UnifiedFrameCollector` and `FramePacket`, and the same ANGLE/Metal renderer
iOS uses.

macOS deliberately does not use the Performance+ shape. There is no JIT
restriction to work around here, so a second process would buy crash isolation
and pay a process, an IPC hop and a second copy of the frame data for it.

Requirements:

- Hardened runtime with `com.apple.security.cs.allow-jit`, plus notarization.
  If the entitlement is missing or the signature does not validate, the profile
  resolver selects a WebKit lane. It does not silently fall back to a jitless
  V8: that configuration deletes WebAssembly outright (`typeof WebAssembly ===
  "undefined"`, measured), so it is a diagnostic profile and never a default.
- One isolate per Session. Concurrent games do not share a GC or a crash domain.
- The snapshot contains Migo bootstrap only, never user data.
- Code cache keys include content hash, V8 build, flags, ABI and cache schema.
- arm64 on Apple silicon and x86_64 on Intel, as real slices. Rosetta is not a
  substitute for a native slice.

The shipping SDK maps the macOS `MigoEngine` slice to `macos-v8` and records
that product in the assembled receipt. The Apple SDK workflow's separate
`external-frames-diagnostic` package does not establish a working V8 product.
The adjacent ANGLE dylibs must be installed with the generated
`Frameworks/Scripts/embed-apple-angle.sh` helper before signing the host app;
see the package README for the build phase and runtime search path.
