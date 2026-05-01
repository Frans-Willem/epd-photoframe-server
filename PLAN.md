# Cleanup plan

One commit per item, ordered smallest-first. Item 8 (dither refactor) is
the biggest concrete-code win; #9 (release pipeline) is the biggest
overall in scope but optional. Everything before #7 is small polish that
can be cut or reordered freely.

## 1. `seconds_until` use `i64::div_ceil`

**Size:** trivial (1 line). **File:** `src/screen_state.rs:168`.

Replace `(ms + 999) / 1000` with `ms.div_ceil(1000)` (stable since 1.79).
Pure cosmetic.

## 2. Drop stale `#[allow(dead_code)]` on `ColorConfig::rgba`

**Size:** trivial (1 line). **File:** `src/config.rs:248`.

The attribute is leftover — `Self::rgba(...)` is called from `from_str`
(lines 280, 290), so the function isn't dead. Just delete the line and
confirm `cargo build` stays clean.

## 3. Drop `Arc<String>` to `String` for `AlbumClient::share_url`

**Size:** small. **File:** `src/album.rs:26`.

Field type change only. `AlbumClient` is already cloned via the
screen-map's `Arc`, so the inner `Arc<String>` is redundant.

## 4. Single source of truth for `PowerState` variants

**Size:** small. **Files:** `src/main.rs` (enum), `src/mqtt.rs:63-69`
(POWER sensor's `options` literal).

Goal: one place to add a new `PowerState` variant. Without a derive
macro (e.g. `strum::EnumIter`) we still have to list the variants once,
but we can put the list and the string mapping in the same impl block as
the enum, so adding `Charging2` only means touching `main.rs`.

Two pieces on `PowerState`:

```rust
impl PowerState {
    pub const ALL: &[Self] = &[Self::Battery, Self::Charging, Self::Full, Self::Fault];

    pub const fn as_str(self) -> &'static str {
        match self { Self::Battery => "battery", /* … */ }
    }
}
```

Have the `Display` impl forward to `as_str`. In `mqtt.rs`, change
`Sensor.options` from `Option<&'static [&'static str]>` to
`Option<&'static [PowerState]>`, point `POWER.options` at
`PowerState::ALL`, and have `publish_discovery` map `as_str` over them
when building the JSON (a `Vec<&'static str>` allocation at startup is
fine — it runs once per screen on connect).

## 5. Replace `Duration::from_std(...).unwrap_or_default()` pattern

**Size:** small (validation in config + helper + 3 call sites).
**Files:** `src/config.rs`, `src/main.rs:311`, `src/screen_state.rs:182,185`.

`chrono::Duration::from_std` only fails for durations larger than
`i64::MAX` milliseconds (~292 million years). The values at the call
sites are all parsed from config TOML via `humantime`, so the failure
condition can only be triggered by a deliberately malformed config —
e.g. `wake_delay = "1000000000000y"`. A request-time panic on bad config
is a worse error mode than a config-load failure, so:

1. Tighten `deserialize_duration` in `config.rs` to reject any value
   that doesn't round-trip through `chrono::Duration::from_std`. Same
   error path as the rest of the deserializer (returns
   `serde::de::Error`), so a bad value is caught at startup with a
   clear message.
2. With that guarantee in hand, add a helper at the top of
   `screen_state.rs`:
   ```rust
   fn add_std(t: DateTime<Utc>, d: Duration) -> DateTime<Utc> {
       t + chrono::Duration::from_std(d).expect("validated at config load")
   }
   ```
   The `expect` is now genuinely unreachable in normal operation —
   anything that survived config parsing will convert.
3. Replace the three call sites with `add_std(...)`.

Net: panic moves from "could fire on every request once a bad config is
loaded" to "can't fire after config load completes".

## 6. Drop the `?action=next/previous` cache-invalidation question

**Size:** none. No code change.

Resolved on review: only `refresh` invalidates the album cache; `next`
/ `previous` step within the current shuffle. (They still snap onto
newly-arrived photos when the cache is replaced for other reasons —
that's the existing behaviour.) README already describes this
correctly. Item kept in the list as a marker so the question doesn't
re-surface.

## 7. Add minimal CI workflow

**Size:** medium (one new file, ~30-50 lines YAML). **File:**
`.github/workflows/ci.yml` (new).

Triggers on push and PR. Two jobs on stable Rust:
- `cargo build --release --locked`
- `cargo test --locked`

Cache `~/.cargo/registry` and `target/` keyed on `Cargo.lock`. Skip
clippy/fmt gates for now to keep the bar low — can tighten later.

Now that the README invites people to try the project, broken-build PRs
should be caught before merge.

## 8. Refactor `dither::process` strategy match

**Size:** largest code change (~50 lines saved across one file).
**File:** `src/dither.rs`, lines 114-256.

The 9-arm match repeats the same 7-line `decomposer +
DecomposingDitherStrategy::new + diffuse_dither` block; only the
decomposer type and the colour-→-point mapper differ.

Plan:
- Add small factory helpers — `octahedron_with(strategy)`,
  `naive_with(strategy)` — that build the decomposer and propagate the
  construction error.
- Add one generic `run<D, F, P>(decomposer, mapper, matrix, inout)` that
  calls `diffuse_dither(DecomposingDitherStrategy::new(...), ...)` once.
- Each arm collapses to a single line, e.g.
  `Strategy::OctahedronClosest => run(octahedron_with(&palette_points, AxisStrategy::Closest)?, color_to_point, matrix, &mut inout)`.

Saves ~50 lines. The strategy table reads as a flat list.

**Risk:** trait bounds on `Decomposer<P>` may not fan out cleanly across
both the colour path (`color_to_point: Rgb<u8> -> Point3<f32>`) and the
grayscale path (`rgb_to_brightness: Rgb<u8> -> f32`). If `run` ends up
needing two variants, abandon and revert — the win isn't worth a more
complex helper than the original match.

## 9. Release pipeline + versioning *(optional, follow-up)*

**Size:** medium-to-large (multi-platform builds + tag conventions).
**Files:** `.github/workflows/release.yml` (new), `Cargo.toml` (version
bumps).

Open question raised on review: should we publish pre-built binaries
and/or a pre-built Docker image, and adopt explicit versioning?

Plausible shape if yes:

- **Versioning:** SemVer in `Cargo.toml`. Releases are git tags
  `v0.1.0`, `v0.2.0`, … pushed manually. CI keys release jobs off
  `push` events for `v*` tags.
- **Binaries:** GitHub Actions matrix building `linux-x86_64-gnu`
  and `linux-aarch64-gnu` (the latter via `cross` or a native arm64
  runner). Upload as release assets. Mac / Windows skipped — the
  target audience runs this on a server.
- **Docker image:** publish to GHCR
  (`ghcr.io/Frans-Willem/epd-photoframe-server`) via
  `docker/build-push-action`. Tagged with the release version and
  `latest`. Multi-arch via QEMU buildx.
- **README docker-compose example** updated to offer
  `image: ghcr.io/...` as a one-liner alternative to the
  `build: ./epd-photoframe-server` we currently show.

Skip if you'd rather keep distribution as "git clone + cargo or
docker-compose build" — that path stays clean and is what the README
already documents. Adding the pipeline is mostly worthwhile if you
expect non-Rust users to try the project; for a Rust-savvy audience
it's friction without much payoff.
