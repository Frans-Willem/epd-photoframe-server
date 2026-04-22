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
