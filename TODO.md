# TODO

## Variable-bit-depth indexed PNG output (parked)

Implemented on `experiment/variable-bit-depth-png` (commit `d0d047c`):
`DitherBuffer` packs indices MSB-first at 1/2/4/8 bpp depending on the
output palette size, so the 6-colour Spectra 6 palette ships as 4 bpp
(≈½ the raw-pixel payload vs 8 bpp). Blocked on the end devices — they
currently assume 8 bpp indexed PNGs and need a decoder update before
this can merge.

## Battery level reporting

Device has an ADC reading the battery in millivolts. Still open:
- Wire up the device → server report (probably `?battery_mv=…` on the
  existing `GET /screen/{name}` — keep one endpoint, no extra round trip).
- Pick a Li-ion/Li-Po SoC curve. Two reasonable paths:
  - **Datasheet curve** if the cell is known — published discharge
    curves give mV → % under a nominal load.
  - **Empirical calibration**: charge to 4.2 V, let the device run its
    normal cycle, log `(timestamp, mv)` via the report channel, then
    fit a lookup table (e.g. 10 % steps, linear interpolation between).
    More work up front, but matches the actual load profile.
- Decide what to do with the % once we have it: battery icon in the
  infobox, a low-battery overlay, and/or logging for future recalibration.

**Where to convert:** keep mV on the device, do mV → % on the server.
Swapping the curve then doesn't require reflashing, multiple device
types / batteries can be supported by keying on screen name, and the
device stays dumb. The server already processes every request; a
lookup-table is trivial there.

## Degrade to a partial image on soft failures

Today a failure anywhere in the render pipeline means the device gets
an error and no image. For some failures we can still produce something
useful:

- **Weather fetch fails:** render the photo normally, but print the
  error text in the infobox instead of the weather line.
- **Photo fetch/decode fails:** synthesize an image with the error
  message as the content, and still render the infobox over it
  (weather, time, etc.) so the device shows *something*.

In either degraded case, shorten the next-refresh hint to
`min(15 minutes, time until next scheduled rotate)` so we retry sooner
than the normal rotation cadence without spamming on a cron like
`*/5`.

## Refresh-to-wake-up delay

The ESP32-S3's RTC-slow clock runs off an internal 150 kHz RC
oscillator (neither E1002 nor E1004 routes an external 32 kHz XTAL
to pins 22/23 — confirmed from the schematics), spec'd at ±5 %.
Over a ~5 min refresh that's ±15 s of drift; over a 24 h target it's
±72 min uncalibrated, or realistically <±60 s once the firmware
applies the boot-time calibration stored in `LPWR.store1`.

Even with calibration, we'd rather have the device wake *late* than
early — waking early on a 24 h schedule means two fetches for one
scheduled image (both battery-expensive), and the server will just
tell it "come back in a bit" anyway. The cleaner design is a
server-side contract:

- Server decides when the next image is scheduled (e.g. aligned to a
  wallclock schedule — already what `rotate.cron` / `rotate.natural`
  drive).
- Server also picks a "wake-up delay" — how long *after* the
  scheduled regeneration the client should actually wake. This is the
  budget for residual client-clock drift plus server regeneration
  time, so the wake always lands comfortably inside the "new image
  ready" window.
- Wire this over the `Refresh:` header as-is: the client just sees
  `Refresh: <scheduled_interval + wake_up_delay>` and the intermediate
  split is invisible to it. Alternatively, a dedicated header pair
  (`X-Refresh-At`, `X-Refresh-Delay`) would let the firmware log the
  two numbers separately for diagnostics.

Pairs with the firmware-side calibrated-sleep fix (tracked in the
reterminal_e100x TODO): once the client can hit the server's target
within a known envelope, the server can pick a wake-up delay close to
that envelope without risk.

## Calibrate the Spectra 6 palette against the actual panel

Both palettes we dither against today — `spectra6` (the nominal sRGB
values from the Spectra 6 datasheet) and `epdoptimize` (a hand-tuned
variant) — visibly diverge from what the E1002 / E1004 panels
actually render. A print of the same palette from an HP laser printer
also differs substantially from the panels, so the gap isn't just
"paper vs. screen" — it's that we don't have a measured ground truth
for what each of the six pigments looks like on *these* panels.

**Goal:** display the same test image on the e-ink panel and print a
copy on the laser printer, hold them side-by-side under normal
daylight, and have the colours be close enough that they're barely
distinguishable with the naked eye. The print is the fixed reference;
only the palette is tuned until the panel matches it — the dithering
pipeline is not what's off here.

Rough plan:

- Render a test image containing one big solid swatch per palette
  entry (one full-pigment block for each of the six Spectra 6
  colours, sized large enough that the panel displays pure pigment
  with no dithering artefacts).
- Measure the displayed colours — phone camera under controlled
  lighting is a reasonable start; a proper colorimeter / spectro
  would be better if one's available.
- Fit that back into an sRGB palette the dither pipeline can use, so
  the colours we ask for in the quantiser are the colours that come
  out of the panel.
- Store the calibrated palette as a new named variant alongside
  `spectra6` / `epdoptimize` so we can A/B without losing the current
  behaviour.

Worth doing per-panel-type (E1002 vs E1004) since they're different
Spectra 6 modules and may ship with slightly different pigment
balances; possibly even per-unit if variance between two E1002s
turns out to matter.
