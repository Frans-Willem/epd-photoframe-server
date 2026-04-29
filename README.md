# epd-photoframe-server

HTTP server that renders photos from Google Photos shared albums into
dithered PNGs sized and palette-matched for e-paper photo frames. Each
configured screen is a section in `config.toml`; the matching device
polls `/screen/<name>` when it wakes from deep sleep, gets back a fresh
PNG plus a `Refresh` header telling it when to come back.

See `config.example.toml` for the full per-screen option surface.

## `wake_delay`

The `Refresh` header the server sends on every response tells the
device when to wake up for its next fetch. Without extra care, that
time is set to exactly the next scheduled rotation — e.g. if the
schedule is `rotate.cron = "0 0 2 * * *"` (rotate at 2 AM) and the
device polls at 2 PM, the response asks it to wake 12 hours later.

The problem: the ESP32-S3's RTC-slow clock runs off an internal
150 kHz RC oscillator (spec'd at ±5 %), so a 12-hour sleep can land up
to several minutes early. Waking early means the device fetches *the
old image*, gets told to come back in a bit, and then wakes again for
the real new image — two fetches for one scheduled rotation, and
e-paper fetches are battery-expensive.

`wake_delay` fixes this by instructing the device to wake some time
*past* the scheduled rotation rather than exactly at it. With
`wake_delay = "1h"` and a 2 AM rotation, the device is told to wake at
3 AM; even if its clock drifts early and it actually wakes at 2:30 AM,
it still lands after the rotation and gets the new image in a single
fetch. For a rotation that happens overnight the user doesn't care
whether the new image shows up at 2 AM or 3 AM — both are "sometime
during the night" — so the delay is free.

Pick `wake_delay` to comfortably exceed the device's expected drift
over the refresh interval. On an uncalibrated ESP32 RTC that's a few
percent of the interval (so roughly 15–60 min for a daily rotation, a
few seconds for a 5-minute rotation). The default is zero; the device
is then told to wake exactly at the scheduled rotation.

## `battery_indicator`

If the device passes a `battery_pct` query parameter on its request
(e.g. `GET /screen/living-room?battery_pct=72`), and the screen has a
`[screens.battery_indicator]` section configured, the server overlays
a small battery readout on the rendered image. The readout's `style`
chooses between `icon` (a battery glyph alone), `text` (just `72%`),
or `both` (the Android 16-style icon with the percentage number drawn
inside it).

The icon is a filled silhouette — body plus a small terminal nub on
the right — with no outline. `empty_color` fills the silhouette;
`foreground` overlays the charged portion of the body, growing left
to right with a clean vertical edge at the level boundary. For
`style = "both"`, the percentage number (no `%` sign) is centred over
the body and drawn twice with inverted clip masks: in `foreground`
over the empty portion, and in `empty_color` over the charged
portion. This keeps the digits readable on both halves regardless of
where the level boundary falls.

Battery readings are not stored between requests — a request without
`battery_pct` simply gets no overlay. The percentage is computed on
the device; the server treats whatever the device sends as ground
truth and clamps it to `[0, 100]`.

The optional `thresholds` array swaps the level-fill colour (and the
overlay text in `style = "both"`) at low charge — the same idea as
Android's yellow-then-red shift. List `{ below, color }` pairs in any
order; the most restrictive match (lowest `below` such that
`pct < below`) wins. Default Android values mirror the framework
constants `config_lowBatteryWarningLevel = 20` and
`config_criticalBatteryWarningLevel = 10`, with the dark-theme
`Warning` and `Error` colours from `BatteryDrawableState.kt`'s
`ColorProfile`. Note that on a real device the yellow tier triggers
on battery-saver mode rather than a fixed percentage, so the two-tier
threshold form is an approximation of the user-visible behaviour.

## MQTT / Home Assistant

If the top-level `[mqtt]` section is present, the server forwards
device-supplied query-string sensor values to a broker and announces
them once on startup via Home Assistant's MQTT discovery protocol.
Each screen becomes one HA *device*; the per-screen `publish_*` flags
decide which sensors hang off it:

| Flag | Query params consumed | HA sensors |
|---|---|---|
| `publish_battery` (default `true`)  | `battery_mv`, `battery_pct` | Battery voltage (mV, `voltage` class), Battery (%, `battery` class) |
| `publish_temperature` (default false) | `temp_c`                     | Temperature (°C, `temperature` class) |
| `publish_humidity` (default false)    | `humidity_pct`               | Humidity (%, `humidity` class) |
| `publish_power` (default false)       | `power`                      | Power (`enum` class, options `battery` / `charging` / `full` / `fault`) |

Discovery topic: `<discovery_prefix>/sensor/epd_photoframe_<slug>/<key>/config`,
state topic: `<state_prefix>/<screen>/<key>` — where `<slug>` lowercases
the screen name and replaces non-alphanumerics with `_` (HA's `node_id`
restriction), while the state-topic uses the original screen name.
A request like

```
GET /screen/living-room?battery_mv=3660&battery_pct=38&temp_c=21.87&humidity_pct=45.64&power=charging
```

triggers a `try_publish` per enabled-and-present sensor (fire-and-forget;
the response never blocks on the broker). Connection failures are
logged and retried by rumqttc's eventloop in the background.
