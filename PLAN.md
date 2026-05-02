# Overlay refactor + multi-day weather forecast

Two phases, in order:

1. **Overlay abstraction** — pull the existing battery indicator and
   infobox out from inline calls in the request handler and behind a
   uniform `Overlay` trait. Preprocess (e.g. weather fetch) runs in
   parallel with photo retrieval.
2. **Multi-day weather forecast** — lands as added configuration and
   render code on the new `Infobox` overlay; the existing
   single-day infobox is preserved as the default combination.

A reference mockup of the new weather variants on E1004 lives at
`/tmp/e1004_forecast_layouts.html` (built by `/tmp/build_layouts.py`).

---

# Phase 1 — Overlay abstraction + unified Pixmap pipeline

## Goal

Two tightly-coupled refactors that ship together:

1. **Decouple individual overlay components** (currently
   `battery_indicator` and `infobox`/weather) from the screen render
   pipeline behind a uniform trait. Each overlay declares its own
   async preprocess (which can fetch external data, snapshot sensor
   state, etc.) and a synchronous render that draws onto the canvas.
2. **Make `tiny_skia::Pixmap` the canonical pipeline format** instead
   of `image::RgbImage`. A single Pixmap is allocated per request,
   the background and photo are painted into it, overlays render
   onto it, and the dither pipeline reads from it via `epd-dither`'s
   `ImageReader` trait. The current `rgb_to_pixmap` / `pixmap_to_rgb`
   round-trips inside the infobox disappear.

## Pipeline

For each screen request:

1. Allocate one `Pixmap` at the screen's `(width, height)`.
2. In parallel:
   - Photo retrieval → resize → paint onto the Pixmap (background
     colour first, then the resized photo on top).
   - `preprocess` for every overlay (single `tokio::join!` /
     `try_join_all`).
3. Once both are done: call each `ReadyOverlay::render(&mut Pixmap)`
   in list order.
4. Pass the Pixmap (via a thin `PixmapReader` wrapper implementing
   `ImageSize + ImageReader`) to the dither pipeline. Dither output
   format unchanged.

Net latency win: weather fetch (currently inside infobox rendering,
serialised after photo retrieval) now overlaps with the photo
download. Battery indicator preprocess is trivial today but the
uniform model leaves room for future overlays that need their own
async work (calendar, notifications, etc.).

Memory cost of the Pixmap-as-canonical change: 1 byte/pixel extra
(RGBA vs RGB). On E1004 that's ~1.9 MB more per in-flight request
— negligible.

## Trait sketch

Approximate shape — finalise during implementation.

```rust
pub struct OverlayContext<'a> {
    pub now:         DateTime<chrono_tz::Tz>,
    pub sensors:     &'a SensorState,        // battery %, etc. from MQTT
    pub http:        &'a reqwest::Client,
    pub canvas_size: (u32, u32),
}

#[async_trait]
pub trait Overlay: Send + Sync {
    async fn preprocess(&self, ctx: &OverlayContext<'_>)
        -> Box<dyn ReadyOverlay + Send>;
}

pub trait ReadyOverlay {
    fn render(&self, canvas: &mut tiny_skia::Pixmap);
}
```

`OverlayContext` carries **only request-time state** (current
timestamp, latest sensor snapshot, shared HTTP client, target canvas
size). Anything from the screen or per-overlay config that the
overlay needs — timezone, position, lat/long, layout choices,
thresholds — is captured by the overlay struct at construction time
(during config load), not looked up on every preprocess call.

A screen holds a `Vec<Box<dyn Overlay>>` built once at config load.
Order in the vec = render order (later overlays draw on top).

## Layout: taffy + a `Drawable` context enum

Decision recorded from the SVG-library investigation: layout uses
**[taffy](https://github.com/DioxusLabs/taffy)**, rendering stays in
`tiny-skia` + `ab_glyph`, and every node in the taffy tree carries an
optional `Drawable` context describing how it should be painted:

```rust
enum Drawable {
    Text       { content: String, font: FontKey, size: f32 },
    Icon       { glyph: char, size: f32 },
    Background { color: Color, radius: f32 },     // parent decoration
    // future: Border, Divider, ...
}

impl Drawable {
    /// Intrinsic size — used by taffy's measure callback for leaves.
    /// Backgrounds return ZERO since they're sized by their parent box.
    fn measure(&self) -> taffy::Size<f32> {
        match self {
            Drawable::Text { content, font, size } => measure_text(content, *font, *size),
            Drawable::Icon { glyph, size }         => measure_glyph(*glyph, *size),
            Drawable::Background { .. }            => taffy::Size::ZERO,
        }
    }

    /// Paint at the absolute (x, y) computed by taffy. `w`/`h` are the
    /// node's computed box size — only used by Background.
    fn draw(&self, canvas: &mut tiny_skia::Pixmap, x: f32, y: f32, w: f32, h: f32) {
        match self {
            Drawable::Text { content, font, size } => draw_text(canvas, x, y, content, *font, *size),
            Drawable::Icon { glyph, size }         => draw_glyph(canvas, x, y, *glyph, *size),
            Drawable::Background { color, radius } => paint_rounded_rect(canvas, x, y, w, h, *radius, *color),
        }
    }
}
```

With that, the integration with taffy is one-liner closures:

```rust
tree.compute_layout_with_measure(
    root, Size::MAX_CONTENT,
    |_known, _avail, _id, ctx, _style| ctx.map(Drawable::measure).unwrap_or(Size::ZERO),
).unwrap();

walk(&tree, root, ox, oy, &mut |x, y, w, h, ctx| ctx.draw(canvas, x, y, w, h));
```

Leaves carry `Text` / `Icon` and contribute their measured size.
Parents that want a background carry `Background` — attached via
`set_node_context` on a `new_with_children` node, so they're sized
normally by their children + style. Layout computes the tree; a
single uniform walk visits every node and calls `draw()` on its
`Drawable` (parent before children, so backgrounds end up
underneath).

Each overlay's `render` function thus has a consistent shape:
build a small taffy tree, compute layout, walk and paint. **Adding a
new visual primitive is one new `Drawable` variant + two arms in the
`measure`/`draw` impl — no change to the measure callback, the
walker, or any overlay that doesn't use it.**

## Concrete overlays

- **`BatteryIndicator`** — preprocess just snapshots the relevant
  fields from `ctx.sensors`. Render is the existing
  `battery_indicator::apply` body, unchanged.
- **`Infobox`** — preprocess fetches weather (skipped if
  `weather_layout = none`); also captures `ctx.now` so render uses a
  consistent timestamp. Render produces the header + weather sections
  per the layout config (Phase 2).

## Errors

`preprocess` should always succeed — failed external fetches produce
a `ReadyOverlay` that renders an error indicator (matches current
"Weather error" text behaviour). This keeps the pipeline simple
(no `Result`-typed joins) and gives users feedback on the screen
when something's wrong, instead of failing the whole request.

If a hard internal error happens (e.g. the icon font failed to
load), `preprocess` returns a `ReadyOverlay` whose `render` is a
no-op — the photo just renders without that overlay.

## Configuration

No new top-level config concept. The screen continues to declare its
overlays via the existing named sections (`[screens.X.infobox]`,
`[screens.X.battery_indicator]`); the screen builder turns whichever
sections are present into the `Vec<Box<dyn Overlay>>`. So the user-
facing config doesn't change for Phase 1.

## Open questions (Phase 1)

- **Z-order across overlays**: render in list order is fine for the
  current two; revisit if/when overlays start overlapping.
- **Preprocess cancellation**: if photo retrieval fails, do we cancel
  in-flight overlay preprocesses or let them finish? Letting them
  finish is simpler and lets cached state (e.g. weather) be reused
  by the next request — recommend that.
- **Async-trait dependency**: `async_trait` macro vs native async-fn-
  in-trait. Native works on stable but trait objects still need
  `Pin<Box<dyn Future>>` boilerplate; `async_trait` keeps the trait
  declaration readable. Defer the call to implementation.

---

# Phase 2 — Multi-day weather forecast

## Goal

Decouple the infobox into two independently-configured sections —
a **header** (day / date text) and a **weather** panel — and add
multi-day forecast variants for the larger E1004 display. The
existing single-day infobox is one specific combination of the two
sections; the default config reproduces it exactly.

This work lands inside the `Infobox` overlay introduced in Phase 1.

## Configuration

Two new fields on `InfoboxConfig`:

```toml
[screens.living_room.infobox]
header_layout  = "day-date"   # none | date | day | day-date
weather_layout = "one"        # none | one | one-plus-four | five
```

Defaults: `header_layout = "day-date"`, `weather_layout = "one"` —
together this reproduces the current single-day infobox exactly, so
existing configs are unaffected.

If both are `none`, the infobox is not rendered at all.

Naming follows the existing `kebab-case` convention used by
`Position` and `Units` (so the Rust variants are e.g.
`HeaderLayout::DayDate` and `WeatherLayout::OnePlusFour`, with just
`#[serde(rename_all = "kebab-case")]` on each enum — no per-variant
renames).

## Conventions

These apply to all multi-day rendering and were chosen during design
review:

- **Max temperature on top, min below** — matches iOS Weather,
  AccuWeather, weather.com daily rows.
- **3-letter weekday labels** (`Mon`/`Tue`/.../`Sun`) — single-letter
  labels are ambiguous (`T`/`T`, `S`/`S`).
- **min and max on separate lines**, not as a `6/14°` range.
- **Weather glyph mapping unchanged** — new code reuses the existing
  `wmo_icon()` codepoints and the bundled
  `LiberationSans-Bold.ttf` / `WeatherIcons-Regular.ttf`.
- **All sizes derived from `text_px`** (the existing
  `max(min(w,h) × 0.05, 12.0)` rule) so layouts scale on smaller
  displays. The pixel values below are the E1004 instantiation
  (`text_px = 60`); implementation should express them as ratios.

## Header sections (`header_layout`)

All lines: LiberationSans-Bold at `text_px` (60 px on E1004),
left-aligned, with `line_gap` (12 px) between them.

| Value     | Lines rendered          | Nominal section height (E1004) |
|-----------|--------------------------|--------------------------------|
| `none`    | —                        | 0                              |
| `day`     | `Tuesday`                | 60                             |
| `date`    | `5 May 2026`             | 60                             |
| `day-date` | `Tuesday` / `5 May 2026` | 132                            |

Section width = widest line's rendered width.

## Weather sections (`weather_layout`)

### `none`
Nothing rendered. Skip the weather fetch entirely.

### `one` (default)
Single line: weather icon at `1.3 × text_px` (78 px) plus
`icon_gap` (18 px) plus `min–max°C` text at `text_px` (60 px),
sharing a baseline. Falls back to "Weather error" text on fetch
failure (existing behaviour).

### `one-plus-four`
The `one` weather line on top, then a 16 px gap, then a row of
4 compact day-cells covering the next 4 days.

Each cell, contents centred and stacked top-to-bottom:

| Element  | Font size (E1004) | Font                  |
|----------|-------------------|------------------------|
| weekday  | 44 px             | LiberationSans-Bold    |
| icon     | 56 px             | WeatherIcons-Regular   |
| max      | 32 px             | LiberationSans-Bold    |
| min      | 32 px             | LiberationSans-Bold    |

Internal vertical gaps inside a cell: 8 px after weekday, 6 px after
icon, 4 px after max. Nominal cell size: **96 × 182 px**. Cells in the
row: 4 × 96 with 12 px gaps → row content **420 px wide**.

### `five`
A single row of 5 compact day-cells starting with today; no special
today treatment.

Each cell, stacked:

| Element  | Font size (E1004) | Font                  |
|----------|-------------------|------------------------|
| weekday  | 36 px             | LiberationSans-Bold    |
| icon     | 48 px             | WeatherIcons-Regular   |
| max      | 28 px             | LiberationSans-Bold    |
| min      | 28 px             | LiberationSans-Bold    |

Internal vertical gaps: 8 / 6 / 4 px (same pattern as `one-plus-four`).
Nominal cell size: **80 × 158 px**. Row: 5 × 80 with 10 px gaps →
row content **440 px wide**.

## Box composition

The infobox is a vertical stack of (at most) two sections, in order:

1. Header section (omitted if `header_layout = none`)
2. Weather section (omitted if `weather_layout = none`)

Vertical gap between sections (when both present): `line_gap` (12 px).
Internal padding around the stack: `internal_pad` (36 px) on all sides.
Background and rounded corners as today (`radius = text_px × 0.6`).

Box dimensions:
- Width  = max(section widths) + 2 × `internal_pad`
- Height = Σ(section heights) + (n − 1) × `line_gap` + 2 × `internal_pad`

The box is anchored via the existing `Position` mechanism (any corner /
edge / centre).

## Default reproduces current behaviour

`header_layout = day-date` + `weather_layout = one`: header gives the
two `Tuesday` / `5 May 2026` lines, weather gives the icon + range
line, separated by `line_gap`. This is structurally what
`infobox::render` does today. Implementation must verify rendered
output is pixel-identical to the current code for the default config.

## Implementation sketch

1. **Config** (`src/config.rs`)
   - Add `weather_layout: WeatherLayout` and `header_layout:
     HeaderLayout` to `InfoboxConfig`, both `#[serde(default)]`.
   - `WeatherLayout`: `None | One | OnePlusFour | Five`,
     `#[serde(rename_all = "kebab-case")]` → TOML
     `none | one | one-plus-four | five`.
   - `HeaderLayout`: `None | Date | Day | DayDate`, same renaming →
     `none | date | day | day-date`.

2. **Weather fetch** (`src/weather.rs`)
   - Generalise `daily()` to fetch N days; Open-Meteo supports
     `forecast_days=N` with the same response shape (longer arrays).
   - Return type becomes `Vec<DailyWeather>` (or wrapped).
   - N is 0 / 1 / 5 depending on `weather_layout`.

3. **Rendering** (`src/infobox.rs`)
   - Restructure `render()` around the section-stack model: compute
     each section's content + bounding box, stack vertically, derive
     overall box dimensions, anchor via `Position`.
   - Helpers shared between layouts: `compact_cell(...)` (the stacked
     `weekday / icon / max / min` block, parameterised on font sizes
     so `one-plus-four` and `five` both use it), `header_lines(...)`,
     `today_weather_line(...)`.

4. **Tests** — render-without-panic for the combinations users will
   actually hit, plus a visual diff against current output for the
   default:
   - default (`day-date` + `one`) — diff against existing rendering
   - `day-date` + `one-plus-four`
   - `day-date` + `five`
   - `none` + `one` (weather only)
   - `day-date` + `none` (header only, no weather fetch)
   - error path: weather fetch fails for `one-plus-four` / `five`.

## Open questions

- **SVG rendering library** (raised on review): **Resolved** — going
  with `taffy` as a layout engine plus an in-project `Drawable`
  context enum for primitives (see *Phase 1 → Layout: taffy + a
  Drawable context enum*). Reasoning, summarised:
  - The pain is *layout* (manual x/y math), not rasterization;
    `resvg`/`usvg` are rasterizers, and SVG's positioning model is
    itself absolute — wouldn't reduce the math we have to do.
  - Investigated alternatives: **decal** (declarative scene → SVG/PNG)
    has the right shape but is too immature for production
    (4 GitHub stars, first released Feb 2026 — single author, ~10
    weeks of releases). **morphorm** is a credible second-place to
    taffy if we ever want a different layout model. **slint-ui** is
    the wrong tool — interactive UI runtime, overkill for headless
    one-shot composition. **cascada** is even younger than decal.
  - **Taffy** is mature (3.1k stars, used by Bevy / Servo / Zed /
    Lapce), layout-only, slots cleanly next to our existing
    `tiny-skia` + `ab_glyph` code. The `Drawable` enum keeps
    rendering primitives small and local, with `measure` + `draw`
    methods so the taffy callbacks are one-liners.
  - Reconsider decal/resvg if user-supplied overlay templates become
    a feature.
- **Precipitation probability** (e.g. `30%` under the icon) — most
  common "next thing" weather apps add. Not in scope yet; flag if
  there's vertical room in `five`'s cells.
- **Colour coding max/min** (red / blue) — available given Spectra 6,
  but adds a config knob and may clash with photos. Not adopting now.
- **Icon legibility at smaller sizes** — `wmo_icon()` glyphs are used
  at 56 / 48 px in the new compact cells (vs. 78 px in the current
  today line). Sanity-check after first implementation that they
  read at those sizes.
- **E1002 with the multi-day layouts** — config doesn't restrict
  layouts by display size. With `text_px = 24` on E1002 the boxes
  would be much smaller and likely unreadable. Document the
  recommendation, or add a runtime warning, or both.

---

# Sequencing

Total: 7 commits across 4 stages, each step independently shippable.
Each step ends with the build green and existing tests passing.
Behaviour changes are explicit in the step description; pure refactors
are called out.

(Stage groupings are sequencing-only; the design *Phase 1* / *Phase 2*
sections above describe the eventual architecture, not the order of
arrival.)

## Stage 1 — Pixmap pipeline (1 commit)

### Step 1 — Pixmap as canonical pipeline format

**Files:** request handler in `src/main.rs`, `src/background.rs`,
`src/infobox.rs`, `src/battery_indicator.rs`, `src/dither.rs`, new
`PixmapReader` adapter.

**Changes:**
- Allocate one `tiny_skia::Pixmap` per request at screen
  `(width, height)`.
- `background.rs` paints the background colour and the resized photo
  directly onto the Pixmap (eliminating the intermediate `RgbImage`).
- `battery_indicator::apply` and `infobox::apply` change signature
  from `&mut RgbImage` → `&mut Pixmap`. Most of their bodies already
  use Pixmap internally; the `rgb_to_pixmap` / `pixmap_to_rgb`
  round-trips inside `infobox.rs` are deleted.
- Implement a thin `PixmapReader<'a>` wrapping `&'a Pixmap` with
  `epd-dither`'s `ImageSize + ImageReader`. Dither reads pixels
  straight from the Pixmap (alpha=255 throughout, so straight RGB).
- Existing overlay render tests adapted to the new buffer type.
- Update `project_state.md` memory: canonical pipeline format is now
  `Pixmap`.

**Acceptance:** Output of the dither pipeline is byte-identical (or
near-identical with documented rounding tolerances) to before this
commit. `cargo test` clean.

## Stage 2 — Overlay abstraction (1 commit)

### Step 2 — Overlay traits + convert existing overlays + parallel preprocess

**Files:** new module (e.g. `src/overlays/{mod,traits}.rs`),
`Cargo.toml` (add `async-trait`), `src/battery_indicator.rs`,
`src/infobox.rs`, `src/main.rs`, `src/weather.rs`.

Trait definition and first implementations land together — splitting
them produces a scaffold commit with dead-code warnings, and you
don't actually know if the trait shape is right until something
implements it.

**Changes:**
- Define `Overlay`, `ReadyOverlay`, `OverlayContext`, and
  `SensorState` per the trait sketch in Phase 1. `ReadyOverlay::render`
  takes `&mut tiny_skia::Pixmap`. Async-trait approach: `async_trait`
  macro for readability (vs native async-fn-in-trait + manual
  `Pin<Box<...>>` boilerplate for trait objects).
- `BatteryIndicator` and `Infobox` implement `Overlay`. Existing
  `apply()` bodies move into `ReadyOverlay::render`.
- Each overlay struct captures its screen-derived config (timezone,
  position, lat/lon, thresholds) at construction.
- Weather fetch moves from inside the request handler to
  `Infobox::preprocess`. Failed fetch yields a `ReadyOverlay` that
  renders the existing "Weather error" message (preserves current
  behaviour).
- Screen builds `Vec<Box<dyn Overlay>>` once at startup from whichever
  named config sections are present.
- Request handler runs `tokio::join!` (or `try_join_all`) on overlay
  preprocesses **in parallel with** photo retrieval. After both
  complete: render overlays in list order onto the Pixmap, then
  dither.
- Render bodies still use the current ad-hoc tiny-skia code (the
  taffy refactor lands in Stage 3).

**Acceptance:** Default config renders pixel-identical output to
before this commit (snapshot test from Stage 1 still passes).
`cargo test` clean. Server-side log timing shows weather fetch
overlapped with photo retrieval.

## Stage 3 — Taffy migration (1 commit)

### Step 3 — taffy + `Drawable` scaffolding + `Infobox` refactor

**Files:** new `src/overlays/drawable.rs`, `src/overlays/mod.rs`,
`src/overlays/infobox.rs`, `Cargo.toml` (add `taffy`).

Trait/scaffolding and first caller land together: the `Drawable`
shape only gets validated by an actual caller using it. The Stage 2
snapshot tests (`tests/snapshots/infobox/{with,without}_weather.png`)
guarantee no pixel drift.

**Changes:**
- Add the `Drawable` enum with variants `Text`, `Icon`,
  `Background`, and `IconText` (the last is a baseline-aligned
  composite for the today weather line — icon + label sharing a
  baseline, where flexbox baseline alignment is fiddly across fonts
  with different metrics). Each variant carries its colour.
- `impl Drawable { fn measure(&self) -> taffy::Size<f32>; fn
  draw(&self, canvas: &mut Pixmap, x, y, w, h); }` — methods
  delegate to the existing `draw_line` / `paint_rounded_rect`
  helpers in `src/draw.rs`.
- Add `walk(&tree, root, ox, oy, &mut visit)` helper: depth-first
  visitor that calls a closure on every node with context (parent
  before children, so backgrounds end up underneath).
- Refactor `Infobox::render` to: build a small taffy `Column` tree
  with the rounded `Background` on the root and `Text` / `Text` /
  `IconText` children, compute layout, walk and paint.

**Acceptance:** Snapshot tests for both `with_weather` and
`without_weather` pass without regenerating. `cargo build` and
`cargo test` clean.

## Stage 4 — Multi-day layouts (4 commits)

### Step 4 — Add layout config + wire single-day combinations

**Files:** `src/config.rs`, `src/overlays/infobox.rs`.

Config plumbing and the rendering wiring land together — adding
fields nothing reads is the same scaffold-only pattern we've avoided
in the previous stages.

**Changes:**
- Add `HeaderLayout` (`None | Date | Day | DayDate`, default
  `DayDate`) and `WeatherLayout` (`None | One | OnePlusFour | Five`,
  default `One`) enums with `#[serde(rename_all = "kebab-case")]`
  and `Default` impls. Add the matching `#[serde(default)]` fields
  to `InfoboxConfig`.
- `Infobox::render` honours both fields for the single-day
  combinations: `None | Date | Day | DayDate` × `None | One`. Each
  section's leaves are pushed onto a `Vec<NodeId>` conditionally;
  when both sections are empty the render is a no-op (no Pixmap
  writes).
- `OnePlusFour` and `Five` arms fall through to the same single
  weather line as `One` for now — a placeholder. Steps 6–7 replace
  these arms with the multi-day tree-builders.
- `Infobox::preprocess` skips the Open-Meteo fetch entirely when
  `weather_layout = none`. `ReadyOverlay::degraded` becomes the
  separate `weather_failed` flag rather than `weather.is_none()`,
  so "weather not requested" doesn't read as degradation.

**Acceptance:** Existing `with_weather` / `without_weather`
snapshots unchanged (default config). New snapshots for
`header_only`, `weather_only`, `day_only_header`,
`date_only_header`. `empty_layout_is_a_noop` asserts the canvas
stays transparent. `cargo test` clean.

### Step 5 — Generalise weather fetch + implement `one-plus-four`

**Files:** `src/weather.rs`, `src/overlays/infobox.rs`.

The fetch generalisation, the `compact_cell` tree-builder, and the
first layout that uses both ship together — adding a helper with no
caller would just be scaffolding.

**Changes:**
- Rename `weather::daily()` → `weather::forecast(days)` returning
  `Vec<DailyWeather>`. `Infobox::preprocess` calls with N derived
  from `weather_layout` (1 for `One`, 5 for `OnePlusFour` and
  `Five`); `WeatherLayout::None` skips the call entirely.
- Change `ReadyInfobox::weather` from `Option<DailyWeather>` to
  `Vec<DailyWeather>` (index 0 = today).
- Add `compact_cell` tree-builder for one stacked
  `weekday / icon / max / min` block; sizes expressed as ratios of
  `text_px` per the Phase 2 spec. Add `compact_cell_row` that takes
  the today timestamp and a slice of future days, computing each
  cell's weekday from `today + (i + 1)` days.
- `WeatherLayout::OnePlusFour` arm: today's icon+range line followed
  by the 4-cell row.

**Acceptance:** Existing snapshots unchanged. New
`infobox/one_plus_four.png` snapshot rendered against an E1004-shaped
canvas (1200×1600) since the row needs more horizontal room than
the 800×600 test canvas gives.

### Step 6 — Implement `weather_layout = five`

**Files:** `src/overlays/infobox.rs`.

**Changes:**
- Refactor `compact_cell` / `compact_cell_row` to take a `CellStyle`
  struct (font sizes + cell gap + extra-top-margin) so the same
  builder can produce both `one-plus-four` and `five` rows. Add
  `CellStyle::one_plus_four(text_px)` and `CellStyle::five(text_px)`
  factory methods baking in the per-layout proportions.
- `compact_cell_row` takes the `first_date` for the leftmost cell —
  `one-plus-four` passes `today + 1 day`, `five` passes `today`
  itself (no special today block).
- `WeatherLayout::Five` arm: a single 5-cell row, no today line.
  Renders nothing if weather is empty.

**Acceptance:** New `infobox/five.png` snapshot. Existing snapshots
unchanged.

### Step 7 — Documentation

**Files:** `README.md`, `config.example.toml`.

**Changes:**
- README: brief description of the overlay model and the new infobox
  layout options.
- `config.example.toml`: examples for each layout combo a user is
  likely to reach for, including a comment about which layouts suit
  which display.

**Acceptance:** N/A (docs only).
