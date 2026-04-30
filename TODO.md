# TODO

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

## Move firmware-supplied params from query string to headers

The growing list of values the firmware sends (`battery_pct`,
`battery_mv`, `temperature_c`, `humidity_pct`, `power`, `action`)
is making URLs unwieldy. Move them to custom request headers so
the firmware can attach them to a plain GET against any PNG
server — including a generic static-file host serving up a PNG —
and have the metadata simply be ignored by anything that doesn't
care.

- Header names: kebab-case, no `X-` prefix (RFC 6648). E.g.
  `Battery-Pct`, `Battery-Mv`, `Temperature-C`, `Humidity-Pct`,
  `Power`, `Action`.
- Server reads both headers and query string; query string wins
  when both are present, so dropping `?action=refresh` in a
  browser still works for debugging.
- POST is *not* an option — generic static servers respond to
  POST with 405, breaking the "point the firmware at any PNG
  URL" use case.
