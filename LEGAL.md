# Legal Notice

> This document is the authoritative English version. It is not translated,
> to avoid the legal risk of a translation drifting from the source text.

This document clarifies the legal and licensing position of the Migo runtime.

## License

Migo is **source-available** under the [Business Source License 1.1](LICENSE) (BSL 1.1).

- **Change Date:** each release carries its own, stamped as **four years after that release was published**, and the currently stamped date is **2030-09-26** (four years after v0.9.6). Four years is the maximum BSL 1.1 allows: its Terms convert a version on the Change Date or on the fourth anniversary of that version's own publication, whichever comes first, so a later date would grant nothing. Every release re-stamps the date, which is checked by `scripts/test-license-change-date-contract.sh`.
- Until a version's Change Date, use of that version is governed by the BSL 1.1 terms and the Additional Use Grant stated in [LICENSE](LICENSE).
- **Production use is free for small entities.** The Additional Use Grant lets you ship Migo inside your own app at no cost while you are under USD 1,000,000 in annual gross revenue and under 3,000,000 monthly active users. Above either threshold, production use needs a commercial license — see [COMMERCIAL.md](COMMERCIAL.md).
- **Non-production use is unrestricted at any scale.** Reading, auditing, building, testing, benchmarking, modifying and porting the source is granted to everyone by the BSL Terms, with no revenue or user thresholds.
- "Source-available" is **not** the same as "open source under an OSI license" today; after a version's Change Date, that version becomes Apache-2.0.

Please refer to [LICENSE](LICENSE) for the authoritative and binding terms. This document is a plain-language summary and does not override the license text.

### Hosting the engine binary to deliver it into your own app

Migo's Android SDK can be integrated without packaging `libmigo.so` in your APK,
so that users who never open a mini-game never download it (see
`MigoNativeLoader`). Doing that means putting the engine binary somewhere your
app can fetch it — a Play Feature Delivery module, or, on stores that have no
such mechanism, storage you control.

**That is a permitted way to ship Migo inside Your App under the Additional Use
Grant, not a Competitive Offering.** The Use Limitation's first example targets
distributing Migo *as* a library or SDK to third parties; an endpoint that exists
to deliver the engine into your own application is the opposite of that, even
though the bytes travel separately from the rest of your app. Two things keep it
on this side of the line, and both are conditions of this clarification:

- the endpoint serves your app's own delivery, not third parties looking for a
  mini-game engine to build on; and
- you do not present the hosted file as an SDK, a library, or a general-purpose
  runtime download.

This paragraph states the licensor's intent. It is a clarification of the
existing grant and does not enlarge it; where it and [LICENSE](LICENSE) differ,
the license text governs.

## The Migo Name and Logo

"Migo" and the Migo logo are trademarks of the Migo Authors. The BSL 1.1 grant covers the **software**; it grants no rights in the name or logo (see the Terms section of [LICENSE](LICENSE)).

Concretely, and independent of what the software license permits:

- **Permitted without asking.** Referring to Migo by name to say what your product uses or is compatible with — "built on Migo", "runs Migo", "compatible with Migo" — in plain, truthful, descriptive terms. Reproducing this repository with its name and notices intact.
- **Not permitted without written permission.** Naming your own product, service, company, domain, app-store listing or package "Migo" or a confusingly similar variant; using the logo as your own mark or in a way that implies you are Migo or are endorsed by us; and describing a modified build as "Migo" without making clear it is modified and not the official distribution.
- **Forks.** The license lets you fork and modify the code. It does not let you keep the name. A fork must be released under a name that is not confusable with Migo, and must not present itself as the official Migo, as "Migo-compatible" in a certification sense, or as passing any Migo conformance criteria.

If in doubt, ask at **licensing@minigame-labs.com** — permission for reasonable uses is normally straightforward.

## Third-Party Trademarks and API Compatibility

Migo implements a mini-game API surface that is **compatible in style** with mainstream mini-game platforms. To be precise about what this means:

- References in this project to a **mainstream mini-game API** (or similar) denote a **publicly observable API specification / shape**, used solely for interoperability so that existing games can run unmodified. They do **not** refer to, invoke, or imply any affiliation with, endorsement by, or licensing from any third-party brand, platform, or company.
- Migo is **not** affiliated with, sponsored by, or endorsed by any mini-game platform vendor.
- All product names, logos, and brands referenced anywhere in this project are property of their respective owners and are used, where used at all, for **identification purposes only**.
- "Business Source License" is a trademark of MariaDB Corporation Ab.

## Test and Demo Content

Any games, assets, or sample content used for benchmarking, demos, or documentation are used only when **legally licensed for redistribution** (e.g., MIT, Apache-2.0, Unlicense, WTFPL) and with their original license preserved.

- Content under copyleft licenses incompatible with redistribution in source-available materials (e.g., GPL) is **not** bundled or used in promotional materials.
- Commercial intellectual property (game titles, characters, art) is **never** redistributed; where such names appear in third-party reference repositories, they are renamed or excluded.
- Migo does **not** distribute, decompile, or repackage proprietary game archives obtained from any platform.

## Third-Party Components

Migo is built on third-party open-source software. The complete list of dependencies and their licenses is in [NOTICE](NOTICE).

## Contributions

Contributions are accepted under the project's [Contributor License Agreement](CLA.md). See [CONTRIBUTING.md](CONTRIBUTING.md) for details.

## Security

To report a vulnerability, follow the process in [SECURITY.md](SECURITY.md). Do not open public issues for security reports.

---

*This notice is provided for informational purposes and does not constitute legal advice. For commercial licensing, or for any use beyond the BSL 1.1 grant, see [COMMERCIAL.md](COMMERCIAL.md).*
