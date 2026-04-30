# TODO

## Calibrate the Spectra 6 palette for E1002

Upstream `epd-dither` now ships measured Spectra 6 palettes from the
2026-04-30 calibration session, exposed here as `spectra6-d50*` /
`spectra6-d65*`. That session was run on an **E1004** panel only —
E1002 may have a slightly different pigment balance, so the same
palette may not match a print held next to the panel as closely there.

When an E1002 is available:

- Run the same calibration capture on it.
- Compare side-by-side under daylight against a laser-printed reference.
- If the E1004 palette is good enough on E1002 too, note that here and
  close this out. Otherwise upstream a per-panel variant (e.g.
  `SPECTRA6_E1002_*`) and expose it in `config.rs` alongside the
  existing variants.
