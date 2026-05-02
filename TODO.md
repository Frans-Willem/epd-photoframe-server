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
- `HDR_L_0.png` — high-quality blue-noise texture, sourced from the
  upstream `epd-dither` repo. Check `epd-dither`'s licensing terms
  and propagate appropriately.

Two reasonable shapes:
- A `LICENSES/` directory with the verbatim upstream licence texts
  plus a top-level `NOTICE` referencing each asset and its licence.
- An "Asset attribution" section in the README pointing to upstream
  for each asset.

The directory approach is more explicit and survives the README
being rewritten; the README section is lighter.
