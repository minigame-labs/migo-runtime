# MigoAppleWebKit

Lane 1: `WKWebView` runs the content's JavaScript and WebKit renders it.

**Status.** The host side named below has landed: `MigoWebKitSession` owns the
WebView's lifecycle and counts what it observes (provisional starts, commits,
finishes, both failure kinds, content-process terminations), and
`MigoWebKitContentOrigin` serves the per-game origin and accounts for what it
delivered against what it promised. What is not done is the half that needs a
paid Apple developer account: getting 4.7.2 through review.

Most of this lane already exists outside this directory. The content-facing
surface is `migo-web-adapter`, which maps `migo.*` onto a DOM canvas and is
already shipped for the web. What is left here is the host side: the
`migo.*` low-frequency bridge for platform capability, permissions, payment and
files; the WebView lifecycle; per-game origin, CSP, navigation
policy, permission allowlist and storage quota; and recovery from
`webViewWebContentProcessDidTerminate:` as a new runtime generation rather than
as an ordinary navigation failure.

That is why this lane ships before Performance+ despite being the slower one.
It is the cheapest thing that can be submitted to App Review, and App Review is
the risk that can end the project without any engineering being wrong: App
Store guideline 4.7 permits HTML5 mini games, but 4.7.2 requires Apple's prior
permission before native platform APIs are exposed to downloaded software. That
question has to be answered by a real submission, not by reading the guideline.

Constraints:

- JavaScript, WebAssembly, Canvas and WebGL all stay in WebContent. Sending
  draw calls out to native through a script message is the worst possible
  bridge and is not an optimisation to reach for later.
- Host process, WebContent and GPU process memory are counted separately.
  Reading only the App column in Xcode systematically understates the total.
- No reliance on multiple `WKProcessPool` instances for isolation; Apple has
  marked explicit pools as no longer having that effect.
