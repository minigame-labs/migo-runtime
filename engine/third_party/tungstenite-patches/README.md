# Tungstenite RFC7692 framing hook

Published tungstenite 0.30.0 is retained byte for byte apart from the recorded
patch. The patch permits an external codec to receive compressed data fragments,
with a separate encoded-message budget. It retains masking, reserved bits,
control frames and fragment sequencing; it neither negotiates extensions nor
implements DEFLATE. The default configuration continues to reject RSV1.

Migo's services layer must validate the response against the exact offer before
using this mode, enforce decoded size/expansion/CPU budgets, and validate text
only after decoding. Existing upstream inline tests and Autobahn client sources
remain available; no extra server, TLS or compression feature is enabled by the
patch. scripts/test-websocket-deflate-contract.sh verifies the published hash,
ordered patch replay, Cargo selection and behavioral regressions.
