# TODO

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
