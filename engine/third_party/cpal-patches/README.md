# CPAL 0.18.2

This is the published crate, checksum-pinned in `UPSTREAM`, with four patches:

1. `macos-loopback` gates system-output capture, its process-tap lifetime and
   its direct CoreAudio ObjC/Foundation feature requests. The new default keeps
   upstream's existing capture behaviour. Migo requests `default-features=false`
   because it uses device output. Normal physical-device input remains available;
   capture from an output-only device is explicitly refused when disabled.
   AudioToolbox still transitively enables CoreAudio's base `objc2` type bridge;
   the graph gate verifies capture is disabled, not that every newer type
   declaration disappeared. Actual final Mach-O references need Apple checks.
2. Preserve the published `Cargo.lock` as a tracked provenance input despite the
   upstream maintainer's local ignore rule.

3. Keep requested playback independent of iOS system interruption. CPAL owns
   activation, the AudioUnit and ShouldResume; a denied end waits for an explicit
   request. Play during interruption is deferred; pause revokes automatic resume.
   Idle streams never reactivate the process session, stale ends are ignored,
   and native activation/start failure is reported after releasing the stream
   lock. A narrow CoreAudio Stream method conditionally handles a fresh Migo
   playback request without overriding a later pause. Production policy tests,
   twelve executable mutations and the actual iOS Rust source check cover this;
   no native link/device result is inferred. A single CPAL process observer
   applies native transitions before public listeners, even when the listener
   registered first. Reentrant/concurrent notifications are serialized without
   holding registry locks across clients. Device-free subscriptions have guard
   lifetimes; streams opened during an interruption inherit process state.
   Migo removes its parallel observer class, process-global sender list and
   activation calls, retaining category/input policy and bounded host delivery.

4. `wasapi-mmcss` registers the native WASAPI worker with the existing Windows
   bindings, after setup and before callbacks, and keeps a thread-local guard
   through the worker loop. Configuration failure rolls back; normal exit and
   unwind revert the registration on its owner thread. Refusal is nonfatal and
   has no TIME_CRITICAL fallback. Migo enables only this feature on Windows;
   Linux/Apple keep their prior feature selections, and Android stays on Oboe.
   No audio_thread_priority / D-Bus package is added to the product. Portable
   ownership/mutation/trait checks and a workspace-locked Windows source probe
   cover this; Windows CI additionally runs real registration/reversion APIs.
   An unavailable MMCSS service is recorded as denial, not RT-promotion proof.

This avoids two incompatible CPAL/ALSA `links` cohorts and keeps Android on its
separate Oboe 1.11 backend. Migo adapts to owned `StreamConfig`, plain sample-rate
values and the new device descriptions/error kinds. Xrun, automatic route changes
and realtime-promotion refusal keep the active stream. Its error-kind exchange is
checked lock-free before opening, coalesces diagnostics for the control thread,
and does not format/log in Migo's callback. That statement does not certify every
upstream error-construction path allocation-free.

The source/feature gate is `scripts/test-cpal-backend-contract.sh`; ordinary host
audio tests exercise error semantics and the cold allocation boundary. The source
patch does not itself prove macOS 11 support: actual Apple builds, final symbols,
old-OS playback/route changes and device performance remain delivery gates.
