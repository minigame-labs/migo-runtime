# Symphonia PCM 0.6.1 candidate patch

UPSTREAM pins the unchanged published archive. This fork is selected only by
isolated codec candidates; the product decoder graph has not switched.

The published decoder renders to the reusable audio buffer capacity and ignores
the render result. After a maximum-size packet, shorter packets finish by
constructing an EOF error, which is swallowed. Render exactly the current
packet frame count instead, propagate rendering errors, and reject a packet
ending inside a coded frame. PCM parsing/conversion stays in the codec library.

The WAVE shared-adapter allocation regression demonstrates the old short-packet
heap cost. Numeric/pull/push, partial packet errors and measured allocation
contracts must pass before product selection. Native/device acceptance remains.
