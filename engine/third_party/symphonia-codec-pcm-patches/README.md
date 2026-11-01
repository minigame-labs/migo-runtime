# Symphonia PCM 0.6.1 product and candidate patch

UPSTREAM pins the unchanged published archive. The whole-file WAVE product and
isolated codec candidates select the same patched decoder.

The published decoder renders to the reusable audio buffer capacity and ignores
the render result. After a maximum-size packet, shorter packets finish by
constructing an EOF error, which is swallowed. Render exactly the current
packet frame count instead, propagate rendering errors, and reject a packet
ending inside a coded frame. PCM parsing/conversion stays in the codec library.

The WAVE shared-adapter allocation regression demonstrates the old short-packet
heap cost. Numeric/pull/push, partial packet errors and measured allocation
contracts cover product selection. Native/device and shared scheduling acceptance
remain separate.
