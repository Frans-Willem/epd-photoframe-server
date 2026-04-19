# TODO

## Daily info overlay (date + day + weather, bottom-left)

Render the overlay after `fit` + `background` but before dither, so the text
participates in the output palette.

- Font: `ab_glyph` + bundled TTF (e.g. DejaVu Sans). `font8x8` is too crude at
  1200×1600.
- Weather: [Open-Meteo](https://open-meteo.com) — free, no API key, JSON over
  HTTPS. Config block with `latitude` / `longitude` / units. Cache response
  for ~1h.
- Backdrop: start with a solid white card under the text. Semi-transparent
  scrim and no-backdrop options are worse bets on e-ink (contrast over mixed
  photos is unreliable).
- New deps: `ab_glyph` + a TTF asset; second HTTP call for weather (reuse the
  existing `reqwest::Client` in the album client or give it its own).

Date/time source: use the JSON blob timestamp from the album share page
(position `[2]` in each photo entry, UTC millis). Also available via EXIF
`DateTimeOriginal` but that's local-time-without-TZ. JSON is cheaper and
already being scraped.

## Navigation + caching (prev / next, scheduled rotate)

No history buffer, no persistence. State per screen lives in memory only —
on startup each screen gets a fresh seed and cursor 0:

```
struct ScreenState {
    seed: u64,                    // current day's seed
    seeded_at: DateTime<Utc>,     // when `seed` was generated
    cursor: i64,                  // signed; 0 on each seed rotation
}
```

Reboots lose the current cursor position; that's fine — reboots are rare and
a fresh random photo on startup is acceptable UX.

Current photo is resolved as `perm[cursor.rem_euclid(N)]` where `perm` is a
Fisher-Yates shuffle of `[0..N)` seeded by `seed` and `N` is the current
album size. The permutation avoids the birthday-paradox collisions of a raw
`hash(seed, cursor) % N` — every photo shows up once per cycle.

Endpoints (driven by the frame's three buttons):
- `GET  /screen/{name}`          — renders and serves the current photo. The
  refresh button just hits this — no separate endpoint. A refresh may
  produce a different image if the scheduled rotate fires, the weather
  overlay changes, or the album has grown/shrunk, but the photo selection
  itself is unchanged.
- `POST /screen/{name}/next`    — `cursor += 1`
- `POST /screen/{name}/prev`    — `cursor -= 1`

Scheduled rotate (e.g. 02:00): config a daily time-of-day `rotate_at`. On
every request, find the most recent occurrence of `rotate_at` strictly
before `now`; if `seeded_at < that`, generate a new `seed` and set
`cursor = 0`. No background timer needed — the check happens at request
time, and the rotation only fires when the interval `[seeded_at, now]`
crosses the mark.

Prev/next stay within the current day's shuffle — wrapping is by
`rem_euclid(N)`, so `cursor = -1` loops to today's last photo. We never
preserve past seeds.

Known edge case: if photos are added to the album mid-day, `N` changes and
the permutation reshuffles — a given cursor value can map to a different
photo than it did earlier. Accepted.
