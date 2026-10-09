# Touch probe

Minimal content for verifying that input crosses the C ABI and reaches JS.

The whole screen is one colour and nothing else changes over time, so any pixel
difference is attributable to a touch arriving: red before the first touch, green
while a finger is down, blue after release. That isolation is the point — looking
for a response inside a real game means separating it from the game's own
animation, which is exactly the kind of judgement call that makes an acceptance
test arguable.

Run it against the packaged SDK:

```sh
bash scripts/build-linux-sdk.sh
bash tests/c_host/linux/build-with-pkgconfig.sh

RUN=/tmp/migo-touch-probe
mkdir -p "$RUN/files/migo/games/touchprobe/code"
cp tests/c_host/touch-probe/game.{js,json} "$RUN/files/migo/games/touchprobe/code/"

./tests/c_host/linux/c-host "$RUN/files" touchprobe 25
```

Then click the window. It turns green while held and blue on release, and the log
reports the touch coordinates in CSS pixels — which must equal where you clicked,
or the scale-factor conversion is wrong.
