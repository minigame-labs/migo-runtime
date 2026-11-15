# Legal Notice

> This document is the authoritative English version. It is not translated,
> to avoid the legal risk of a translation drifting from the source text.

This document clarifies the legal and licensing position of the Migo runtime.

## License

Migo is distributed in binary form under the [Migo Binary License 1.0](LICENSE)
(SPDX `LicenseRef-Migo-Binary-1.0`). The runtime's source code is not published.

- **Development and evaluation are free at any scale.** Installing the SDK,
  building your app against it, testing, evaluating and benchmarking it are
  granted to everyone, with no revenue or user thresholds (LICENSE §2.1).
- **Production use is free for small entities.** You may ship Migo inside your
  own app at no cost while you are under USD 1,000,000 in annual gross revenue
  and under 3,000,000 monthly active users (LICENSE §2.2). Above either
  threshold, production use needs a commercial license — see
  [COMMERCIAL.md](COMMERCIAL.md). Crossing a threshold leaves you 90 days to
  arrange one.
- **Running other people's mini-games is use inside your app.** Operating a
  mini-game platform in your app, and giving game developers tools and
  documentation to target it, is covered, as long as the runtime itself stays
  embedded in your app (LICENSE §2.5).
- **Never permitted without a commercial license, at any size:** distributing
  Migo as a standalone SDK, engine or library, or offering it to third parties
  as a hosted runtime service (LICENSE §3(a), "Competitive Offering").
- **Reverse engineering is not permitted**, except where applicable law
  expressly allows it (LICENSE §3(c)).

Please refer to [LICENSE](LICENSE) for the authoritative and binding terms. This
document is a plain-language summary and does not override the license text.

### Versions published under BSL 1.1

Versions up to and including **v0.9.19** were published, in source and binary
form, under the Business Source License 1.1. They remain under that license, as
the license file inside their release assets states. Later versions are
published under the Migo Binary License only.

### Delivering the engine binary into your own app

Migo's Android SDK can be integrated without packaging `libmigo.so` in your APK,
so that users who never open a mini-game never download it (see
`MigoNativeLoader`). Doing that means putting the engine binary somewhere your
app can fetch it — a Play Feature Delivery module, or, on stores that have no
such mechanism, storage you control.

**That is distribution as part of Your App** (LICENSE §2.4), not a Competitive
Offering, on two conditions:

- the endpoint serves your app's own delivery, not third parties looking for a
  mini-game engine to build on; and
- you do not present the hosted file as an SDK, a library, or a general-purpose
  runtime download.

## The Migo Name and Logo

"Migo" and the Migo logo are trademarks of the Migo Authors. The software
license grants no rights in the name or logo (LICENSE §7).

- **Permitted without asking.** Referring to Migo by name to say what your
  product uses or is compatible with — "built on Migo", "runs Migo",
  "compatible with Migo" — in plain, truthful, descriptive terms.
- **Not permitted without written permission.** Naming your own product,
  service, company, domain, app-store listing or package "Migo" or a
  confusingly similar variant; using the logo as your own mark or in a way that
  implies you are Migo or are endorsed by us; and presenting a repackaged build
  as the official Migo distribution.

If in doubt, ask at **licensing@minigame-labs.com** — permission for reasonable
uses is normally straightforward.

## Third-Party Trademarks and API Compatibility

Migo implements a mini-game API surface that is **compatible in style** with
mainstream mini-game platforms. To be precise about what this means:

- References in this project to a **mainstream mini-game API** (or similar)
  denote a **publicly observable API specification / shape**, used solely for
  interoperability so that existing games can run unmodified. They do **not**
  refer to, invoke, or imply any affiliation with, endorsement by, or licensing
  from any third-party brand, platform, or company.
- Migo is **not** affiliated with, sponsored by, or endorsed by any mini-game
  platform vendor.
- All product names, logos, and brands referenced anywhere in this project are
  property of their respective owners and are used, where used at all, for
  **identification purposes only**.
- "Business Source License" is a trademark of MariaDB Corporation Ab.

## Test and Demo Content

Any games, assets, or sample content used for benchmarking, demos, or
documentation are used only when **legally licensed for redistribution**
(e.g., MIT, Apache-2.0, Unlicense, WTFPL) and with their original license
preserved.

- Content under copyleft licenses incompatible with redistribution alongside
  proprietary materials (e.g., GPL) is **not** bundled or used in promotional
  materials.
- Commercial intellectual property (game titles, characters, art) is **never**
  redistributed; where such names appear in third-party reference repositories,
  they are renamed or excluded.
- Migo does **not** distribute, decompile, or repackage proprietary game
  archives obtained from any platform.

## Third-Party Components

Migo is built on third-party open-source software, including V8 and Skia. Each
component remains under its own license, and nothing in the Migo Binary License
restricts the rights that license grants you (LICENSE §4). The complete list,
with license texts, is in [NOTICE](NOTICE); each release also carries
CycloneDX SBOMs for its SDK packages.

## Security

To report a vulnerability, follow the process in [SECURITY.md](SECURITY.md). Do
not open public issues for security reports.

---

*This notice is provided for informational purposes and does not constitute
legal advice. For commercial licensing, or for any use beyond the grants in the
LICENSE, see [COMMERCIAL.md](COMMERCIAL.md).*
