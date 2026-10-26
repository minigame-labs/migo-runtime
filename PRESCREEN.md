# Will my mini-games run on Migo?

You do not have to send anyone your games to find out. This is the tool we would
run, and it runs on your machine.

Three scripts answer three different parts of the question, and none answers
another's:

| | question | needs |
|---|---|---|
| `scripts/prescreen-game.sh` | which APIs does this bundle reference, and does this build publish them? | a checkout; the first run builds a host player |
| `scripts/prescreen-run.sh` | does it actually put a frame on screen, and does anything fail? | an Android device over `adb` |
| `scripts/compat-report.sh` | what does it reach for while it runs, and what does this build say of each? | a checkout; builds a host player |

All three write Markdown. Read them, then decide whether you want to talk to us.

---

## Before you start

```
git clone https://github.com/minigame-labs/migo && cd migo
```

You need `python3` and `bash`. For the run half you also need `adb` and a phone
with USB debugging on.

A "bundle" here is a directory with `game.js` at its root — the same shape your
container already loads.

---

## 1. Which APIs does it need

```sh
bash scripts/prescreen-game.sh /path/to/your/bundle --out report.md
```

If your content is written against the mainstream mini-game global (`wx.*`
rather than `migo.*`), point it at the adapter as well, or `wx` cannot be
answered at all. Build the IIFE bundle from
[migo-wx-adapter](https://github.com/minigame-labs/migo-wx-adapter) (its README
has the one build command) and pass it:

```sh
bash scripts/prescreen-game.sh /path/to/your/bundle \
    --adapter /path/to/migo-wx-adapter.bundle.js --out report.md
```

The first run builds a small Linux player and runs a probe as ordinary content,
because the runtime is the authority on what it publishes — reading the sources
would report what registration *intends*, and the two have disagreed before. That
means the first run needs the host build toolchain from [BUILD.md](BUILD.md) and
takes a few minutes. It is a one-off: dump the surface once and reuse it.

```sh
bash scripts/dump-api-surface.sh --adapter <adapter.js> --out surface.json
bash scripts/prescreen-game.sh <bundle> --surface surface.json --out report.md
```

### How to read that report

**"0 gaps" means every API name this bundle references is one we publish.** It
does not mean the bundle runs. A name existing is not a call succeeding —
`wx.login` exists on every build and still fails without a host auth handler.

There is a row called **"sites this scanner cannot resolve"**. Computed keys,
reflection over the namespace, `eval` — access no static scan can follow. That
row is why the counts above it are a **lower bound rather than a promise**. A
scanner that quietly under-reported would hand you "everything is supported",
and that is the one wrong answer that costs you real money later.

**And it now answers a question it used to leave open.** Some names this build
publishes do nothing: `reportEvent` and its siblings are, in the engine's own
words, "no-op stubs that **silently succeed**". A bundle that depends on them
looks perfect in every count above and still does not work. The report has a
bucket for exactly that — *referenced, published, but a stub* — derived from the
engine sources rather than a hand-kept list, and gated so a rename cannot make a
stub quietly disappear from it. If it says none, it means it checked.

It also reads `game.json` for the one thing there no name scan can see: **plugins**,
which the mini-game platform serves from its own servers to `requirePlugin(...)`.
Migo has no plugin service, so a bundle that declares one and calls `requirePlugin`
does not start as built; one that only declares it is told the declaration may be
unused. The usual one is Cocos Creator's engine, delivered this way when "Separate
Engine" is on; rebuild with it off and the engine ships inside the game.

The report also separates two things people usually conflate: names your bundle
**installs itself** (a polyfill or shim it carries — not a gap, we were never
asked for them), and namespace-shaped text that only ever appears **inside
string literals** (a storage key, an Android package name — not calls). Both are
listed rather than silently dropped, so you can see what was considered.

---

## 2. Does it actually run

```sh
bash scripts/prescreen-run.sh /path/to/your/bundle \
    --package <your.host.package> \
    --activity <the activity that puts the game on screen> \
    --device <adb serial> --secs 20 --out run-report.md
```

It copies the bundle into your host's own private directory with `run-as`
(no root, and the content never leaves the phone), launches, watches for twenty
seconds, and writes what it saw plus two screenshots.

Your host needs to be a **debuggable build** and needs an entry point that can
be started with a game id. If it cannot, the script says so instead of guessing.

### If you have not integrated Migo yet

You do not need to. [`migo-examples`](https://github.com/minigame-labs/migo-examples)
carries a host you can install as-is:

```sh
git clone https://github.com/minigame-labs/migo-examples
cd migo-examples
bash scripts/resolve-migo-artifact.sh android-aar android-java/libs/migo.aar
cd android-java && ./gradlew :app:installDebug

cd /path/to/migo
bash scripts/prescreen-run.sh /path/to/your/bundle \
    --package com.minigame.androiddemo \
    --activity .PrescreenProbeActivity \
    --adapter /path/to/migo-wx-adapter.bundle.js --record \
    --device <adb serial> --secs 20 --out run-report.md
```

`PrescreenProbeActivity` exists **only in the debug build** and is the one
game activity there that adb is allowed to start. That boundary is deliberate:
`android:exported="false"` is the right default for a game activity, and this
example keeps teaching it rather than opening a surface any app on the phone
could call. Deploying a bundle needs `run-as`, which needs a debuggable build
anyway, so a probe that exists only in debug builds is present in exactly the
situations where this half can work at all.

It runs the example's own configuration, host handlers and logging, all inherited
from the example's game activity, so what the probe runs is what a host built from
the example runs. It adds only what the script hands it:

- `--adapter`: the adapter your content is written against, injected before the
  game. wx content needs [migo-wx-adapter](https://github.com/minigame-labs/migo-wx-adapter)'s
  bundle; without it the run stops on `wx is not defined`, which says nothing about
  your bundle. Content written against `migo.*` needs none.
- `--record`: this checkout's compatibility recorder, after the adapter, with the
  log at INFO -- section 4 below.

The probe logs each one it injected with its sha256, and the report calls a prelude
injected only when that line is in the log. A host that ignores the request runs
without it, and the report says so instead of judging your bundle by its absence.

**Unlock the phone first.** An activity launched on a sleeping device pauses
before it ever gets a surface, and a report written from that would say "no
engine session ran" about content that is fine. The script wakes the screen and
refuses to produce a verdict if it cannot.

### If your host injects the compatibility recorder

A host you build can also inject `tools/compat-recorder/recorder.js` as a boot prelude, after
the adapter, and log at `RuntimeConfig.LogLevel.INFO` or lower (a release engine logs at WARN,
and the recorder writes at INFO). The run report then adds a fourth section: what the game
reached for on the device, judged the way section 3 below judges it -- and on a device, unlike
the Linux player, a call the host answers "not supported" is the host's real answer.

To take `--adapter` and `--record` the way the probe does, read `adapter.js` and `recorder.js`
from `<files>/migo-prescreen/` when the boolean extras `migo_prescreen_adapter` /
`migo_prescreen_record` are set, and log `injected <name> sha256=<hex>` for each.

### What it will and will not claim

It will not say "runs" unless all of these hold: your host held the foreground,
an engine session reached `RUNNING` **in that process**, something was painted,
and the two captures differ. Miss any one and it says what it actually saw.

That strictness is not caution for its own sake. Earlier versions of this script
reported "runs" for a bundle that had never been loaded, because a host's own
menu screen is colourful and animated and looks, in a screenshot, exactly like a
running game.

A clean result means nothing broke in twenty seconds, on one device, on one
launch. It is not a certification, and it says nothing about level two.

---

## 3. What it reaches for while it runs

```sh
bash scripts/compat-report.sh /path/to/your/bundle \
    --adapter /path/to/migo-wx-adapter.bundle.js --secs 20 \
    --out reach.md --json reach.json
```

The scan in section 1 reads names off the source; this runs the bundle with a recorder injected behind
the adapter and writes down what it actually did: each API it called (once, the first time), each name it
looked for and did not find, the names it installed itself, the WebGL extensions it asked for and the GL
errors it saw, the time to the end of its entry and to its first frame, and the errors the engine logged.
Each API is then judged by this build's contracts for Android: whether the engine answers it, whether
your host app does (its ads, payments, accounts), whether the conformance suite verified it, and whether it
is deliberately not offered. `reach.json` is the same, for a machine.

**It runs on the Linux player, and the report says so.** Linux has no device services, so a game that
fails to log in here takes a path it would not take on a phone, and calls what that path calls. Treat it as
a first look; the device run in section 2 is the authority on whether the game runs. A call the run did
not reach is not in the report: it is one run, not every path through the game.

The recorder is not part of any shipped host. Off means not injected, so a host that does not inject it
carries none of it.

---

## About your content

The point of shipping you the tool is that this section is short.

- **Your bundle never leaves your machine.** The scan and the Linux run are local; the
  device run copies into your own phone's app sandbox over your own `adb`.
- **Nothing is uploaded.** None of the three scripts has a network path for your content.
- **We never see it** unless you decide to send us the *report*, which contains
  API names, counts and your own screenshots — not your code.

If you would rather we ran it, we can do that under an NDA with the usual terms
(single-purpose use, deletion within an agreed window, never retained for
training or calibration). But you should not need to get that far to learn
whether your catalogue runs.

---

## When the report says something is missing

Send us the report. A missing API name is usually one of three things: something
we have not implemented, something your host is expected to provide through a
handler (login, payment, ads — those are yours by design, see
[COMMERCIAL.md](COMMERCIAL.md)), or an API that platform never had either.

Which one it is changes the answer completely, and it is a five-minute
conversation once the report exists.

`licensing@minigame-labs.com`
