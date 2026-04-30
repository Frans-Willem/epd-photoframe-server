use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Offset, Utc};
use chrono_tz::Tz;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng, seq::SliceRandom};

use crate::config::{Rotate, ScreenConfig};

impl Rotate {
    /// Next scheduled trigger strictly after `after`, in UTC.
    ///
    /// Both parsers have their own timezone story:
    /// - `cron` consumes a `DateTime<Tz>` and returns triggers in that TZ.
    /// - `cron-lingo` only iterates from the system clock (no arbitrary start);
    ///   we use its `assume_offset` to pin the offset and rely on the fact that
    ///   we only ever call this with `after ≈ now`.
    pub fn next_after(&self, after: DateTime<Utc>, tz: &Tz) -> Option<DateTime<Utc>> {
        match self {
            Self::Cron(s) => s
                .after(&after.with_timezone(tz))
                .next()
                .map(|dt| dt.with_timezone(&Utc)),
            Self::Natural(s) => {
                let offset_secs = after.with_timezone(tz).offset().fix().local_minus_utc();
                let offset = time::UtcOffset::from_whole_seconds(offset_secs).ok()?;
                let next = s
                    .iter()
                    .inspect_err(|e| tracing::warn!(error = ?e, "cron-lingo iter failed"))
                    .ok()?
                    .assume_offset(offset)
                    .next()?
                    .ok()?;
                let utc = next.to_offset(time::UtcOffset::UTC);
                DateTime::<Utc>::from_timestamp(utc.unix_timestamp(), utc.nanosecond())
            }
        }
    }
}

/// Per-screen rotation state. Owns the rotation schedule and timezone so
/// callers don't have to thread them through every operation: the seed
/// driving the current shuffle, the navigation cursor, the moment at which
/// rotation last fired (the reference for computing the next trigger), and
/// the schedule + tz used to evaluate it. `last_rotation == None` means the
/// state is uninitialised (the constructor leaves `seed = 0` as a
/// placeholder); the first call to `maybe_rotate` always fires a rotation
/// to establish a valid seed.
pub struct ScreenState {
    seed: u64,
    cursor: i64,
    last_rotation: Option<DateTime<Utc>>,
    rotate: Option<Rotate>,
    tz: Tz,
}

impl ScreenState {
    pub fn new(config: &ScreenConfig) -> Self {
        Self {
            seed: 0,
            cursor: 0,
            last_rotation: None,
            rotate: config.rotate.clone(),
            tz: config.timezone,
        }
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn cursor(&self) -> i64 {
        self.cursor
    }

    fn advance(&mut self, delta: i64) {
        self.cursor = self.cursor.wrapping_add(delta);
    }

    /// The first scheduled rotation moment strictly after `since`. None when
    /// there's no schedule or the schedule has no future triggers.
    fn next_rotation(&self, since: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.rotate
            .as_ref()
            .and_then(|r| r.next_after(since, &self.tz))
    }

    /// Apply rotation, navigation, and (when triggered) snap-to-new in one
    /// atomic state transition, then return the resolved photo index and the
    /// next scheduled rotation moment. `advance` shifts the cursor (1 for
    /// next, -1 for previous, 0 for none). On any non-passive event — a
    /// rotation just fired, `fresh` (refresh), or a non-zero `advance`
    /// (next/previous) — and when `new` is non-empty, the cursor is
    /// advanced further until `resolve_index` lands on one of the new
    /// indices, so the snap persists across subsequent requests rather than
    /// being a one-shot override. Passive polling (plain GET, no rotation)
    /// leaves the cursor where it is.
    pub fn pick_index(
        &mut self,
        now: DateTime<Utc>,
        advance: i64,
        fresh: bool,
        new: &[usize],
        n: usize,
    ) -> (usize, Option<DateTime<Utc>>) {
        let rotated = self.maybe_rotate(now);
        self.advance(advance);
        let snap = (rotated || fresh || advance != 0) && !new.is_empty();
        if snap {
            let new_set: HashSet<usize> = new.iter().copied().collect();
            for offset in 0..(n as i64) {
                let idx = resolve_index(self.seed, self.cursor.wrapping_add(offset), n);
                if new_set.contains(&idx) {
                    self.advance(offset);
                    break;
                }
            }
        }
        let nr = self
            .last_rotation
            .and_then(|since| self.next_rotation(since));
        (resolve_index(self.seed, self.cursor, n), nr)
    }

    /// Reseed and reset the cursor if a scheduled trigger has elapsed since
    /// `last_rotation`, OR if the state is uninitialised
    /// (`last_rotation == None`, seed is the placeholder). Returns true iff
    /// a rotation actually fired.
    fn maybe_rotate(&mut self, now: DateTime<Utc>) -> bool {
        let should_rotate = match self.last_rotation {
            None => true,
            Some(since) => self.next_rotation(since).is_some_and(|next| now >= next),
        };
        if !should_rotate {
            return false;
        }
        let old_seed = self.seed;
        self.seed = rand::rng().random();
        self.cursor = 0;
        self.last_rotation = Some(now);
        tracing::info!(old_seed, new_seed = self.seed, last_rotation = ?now, "rotated screen");
        true
    }
}

/// Fisher-Yates permutation of `[0..n)` seeded by `seed`, indexed by
/// `cursor.rem_euclid(n)`. Panics if `n == 0`.
fn resolve_index(seed: u64, cursor: i64, n: usize) -> usize {
    assert!(n > 0, "resolve_index called with empty album");
    let mut perm: Vec<usize> = (0..n).collect();
    let mut rng = StdRng::seed_from_u64(seed);
    perm.shuffle(&mut rng);
    perm[cursor.rem_euclid(n as i64) as usize]
}

/// Seconds from `now` to `target`, rounded up, clamped at 0.
pub fn seconds_until(target: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    let ms = (target - now).num_milliseconds();
    if ms <= 0 { 0 } else { (ms + 999) / 1000 }
}

/// The absolute moment at which an error response should ask the device to
/// retry. Base is `now + error_refresh`, but never later than the device's
/// normal next-fetch target (`next_rotation + wake_delay`) — pushing past
/// that would have the device skip a scheduled rotation. With no rotation
/// schedule the cap doesn't apply.
pub fn error_refresh_target(
    error_refresh: Duration,
    wake_delay: Duration,
    next_rotation: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let base = now + chrono::Duration::from_std(error_refresh).unwrap_or_default();
    match next_rotation {
        Some(n) => {
            let cap = n + chrono::Duration::from_std(wake_delay).unwrap_or_default();
            base.min(cap)
        }
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::collections::HashSet;

    fn tz() -> Tz {
        "Europe/Amsterdam".parse().unwrap()
    }

    fn config(rotate_toml: &str) -> ScreenConfig {
        toml::from_str(&format!(
            r#"
            name = "x"
            width = 800
            height = 480
            share_url = "https://example.com"
            timezone = "Europe/Amsterdam"
            {rotate_toml}
            "#
        ))
        .unwrap()
    }

    #[test]
    fn resolve_index_is_a_permutation() {
        let seen: HashSet<usize> = (0..10).map(|c| resolve_index(42, c, 10)).collect();
        assert_eq!(seen.len(), 10);
    }

    #[test]
    fn resolve_index_wraps_negative_cursor() {
        assert_eq!(resolve_index(42, -1, 5), resolve_index(42, 4, 5));
    }

    #[test]
    fn first_call_always_rotates() {
        let cfg = config(r#"rotate.cron = "0 0 2 * * *""#);
        let mut s = ScreenState::new(&cfg);
        assert_eq!(s.seed(), 0);
        assert!(s.last_rotation.is_none());
        let rotated = s.maybe_rotate(Utc::now());
        assert!(rotated);
        assert_ne!(s.seed(), 0);
        assert!(s.last_rotation.is_some());
    }

    #[test]
    fn first_call_rotates_even_without_schedule() {
        let cfg = config("");
        let mut s = ScreenState::new(&cfg);
        let rotated = s.maybe_rotate(Utc::now());
        assert!(rotated);
        assert!(s.last_rotation.is_some());
    }

    #[test]
    fn cron_rotate_fires_after_next() {
        let cfg = config(r#"rotate.cron = "0 0 2 * * *""#);
        let tz = tz();
        // Initialise at 20 Apr 12:00 local (first call always rotates).
        let start = tz
            .with_ymd_and_hms(2026, 4, 20, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let mut s = ScreenState::new(&cfg);
        s.maybe_rotate(start);
        let seed_before = s.seed();
        s.advance(3);
        // Advance past 02:00 next day — should fire another rotation.
        let later = tz
            .with_ymd_and_hms(2026, 4, 21, 3, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let rotated = s.maybe_rotate(later);
        assert!(rotated);
        assert_eq!(s.cursor(), 0);
        assert_ne!(s.seed(), seed_before);
        assert_eq!(s.last_rotation, Some(later));
    }

    #[test]
    fn cron_rotate_noop_before_next() {
        let cfg = config(r#"rotate.cron = "0 0 2 * * *""#);
        let tz = tz();
        let start = tz
            .with_ymd_and_hms(2026, 4, 21, 3, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let mut s = ScreenState::new(&cfg);
        s.maybe_rotate(start); // initialise; next trigger is 22 Apr 02:00
        s.advance(5);
        let snap = (s.seed(), s.cursor(), s.last_rotation);
        // 10 h later is still 21 Apr 13:00, before the 22 Apr 02:00 trigger.
        let now = start + chrono::Duration::hours(10);
        let rotated = s.maybe_rotate(now);
        assert!(!rotated);
        assert_eq!((s.seed(), s.cursor(), s.last_rotation), snap);
    }

    #[test]
    fn no_schedule_means_no_rotation_after_init() {
        let cfg = config("");
        let mut s = ScreenState::new(&cfg);
        s.maybe_rotate(Utc::now()); // initial rotation
        s.advance(7);
        let snap = (s.seed(), s.cursor(), s.last_rotation);
        let rotated = s.maybe_rotate(Utc::now() + chrono::Duration::days(365));
        assert!(!rotated);
        assert_eq!((s.seed(), s.cursor(), s.last_rotation), snap);
    }

    #[test]
    fn seconds_until_rounds_up() {
        let now = Utc::now();
        assert_eq!(seconds_until(now, now), 0);
        assert_eq!(
            seconds_until(now + chrono::Duration::milliseconds(1), now),
            1
        );
        assert_eq!(
            seconds_until(now + chrono::Duration::milliseconds(1000), now),
            1
        );
        assert_eq!(
            seconds_until(now + chrono::Duration::milliseconds(1001), now),
            2
        );
        assert_eq!(seconds_until(now - chrono::Duration::seconds(5), now), 0);
    }

    #[test]
    fn error_refresh_target_no_schedule_uses_base() {
        let now = Utc::now();
        let t = error_refresh_target(Duration::from_hours(1), Duration::ZERO, None, now);
        assert_eq!(t, now + chrono::Duration::hours(1));
    }

    #[test]
    fn error_refresh_target_clamps_to_wake_target_when_sooner() {
        let now = Utc::now();
        // Next rotation in 10 min, wake_delay 5 min → cap is 15 min.
        let next_rotation = now + chrono::Duration::minutes(10);
        let t = error_refresh_target(
            Duration::from_hours(1),
            Duration::from_mins(5),
            Some(next_rotation),
            now,
        );
        assert_eq!(t, next_rotation + chrono::Duration::minutes(5));
    }

    #[test]
    fn error_refresh_target_uses_base_when_wake_target_is_later() {
        let now = Utc::now();
        // Next rotation in 6 h → 1 h error_refresh wins.
        let next_rotation = now + chrono::Duration::hours(6);
        let t = error_refresh_target(
            Duration::from_hours(1),
            Duration::ZERO,
            Some(next_rotation),
            now,
        );
        assert_eq!(t, now + chrono::Duration::hours(1));
    }
}
