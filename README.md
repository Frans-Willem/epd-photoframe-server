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
