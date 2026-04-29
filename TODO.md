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

## Run cargo clippy and rustfmt
The tree currently has six pre-existing `clippy::collapsible_if`
warnings in `src/main.rs` — back-to-back forms like
`if cfg.publish.contains(&Publish::Power) { if let Some(v) = q.power
{ … } }` that fold cleanly with `&& let` on the 2024 edition. Once
those are tidied, run `cargo fmt` across the workspace and consider
wiring `cargo fmt --check` and `cargo clippy --all-targets -- -D
warnings` into a pre-push or CI step so new warnings surface
immediately rather than accumulating into another sweep like this
one.

## action=refresh should refresh the album
Today `?action=refresh` is a no-op (see the comment in
`config.example.toml`: "?action=next / ?action=previous step the
cursor within the current shuffle; ?action=refresh is a no-op").
Make it a meaningful escape hatch: drop the cached album contents
and any per-album share-page state for that screen, re-resolve the
share URL, and render against the freshly-fetched list. Useful when
a photo has just been added or removed in Google Photos and the
user doesn't want to wait for the next scheduled rotation.

## Track new photos across rotations
Keep an in-memory per-album record of which photo IDs have been seen
so we can detect newly-added photos. On each rotation (scheduled or
`?action=refresh`-triggered), compare the current share contents
against the seen set; if anything is new, position the cursor on a
freshly-added photo rather than continuing the existing shuffle.
No need to persist this to disk — the server is intended to run
long-term, and forgetting the seen set on a restart is acceptable.
