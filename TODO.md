# TODO

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
