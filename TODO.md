# TODO

## Licensing

### Asset attribution
Bundled assets in `assets/` need explicit attribution and license
texts before a public release. Verify the actual upstream licenses
before writing this up — best-guess noted in parens.

- `LiberationSans-Bold.ttf` — Liberation Fonts project (likely SIL
  Open Font License 1.1 with original Bitstream Vera license terms).
- `WeatherIcons-Regular.ttf` — Weather Icons by Erik Flowers (likely
  SIL OFL 1.1; CSS portion under MIT).

Two reasonable shapes:
- A `LICENSES/` directory with the verbatim upstream licence texts
  plus a top-level `NOTICE` referencing each asset and its licence.
- An "Asset attribution" section in the README pointing to upstream
  for each asset.

The directory approach is more explicit and survives the README
being rewritten; the README section is lighter.

## Features

### Precipitation in the weather forecast
Open-Meteo returns daily precipitation probability and totals; show
them somewhere in the multi-day cells (most weather apps put a `30%`
under or beside the icon). Sanity-check legibility at the smaller
cell sizes before committing to a position.

### Localization
Month names, full weekday names, 3-letter weekday labels, and date
formatting are all hard-coded English in the infobox. Investigate
whether an off-the-shelf i18n crate (e.g. `icu`, `pure-rust-locales`,
`fluent`) handles these cleanly given a `chrono::DateTime<Tz>` and
a locale tag, or whether bundled tables are simpler for the small set
of strings the infobox needs. Locale would presumably be a per-screen
config knob alongside `timezone`.
