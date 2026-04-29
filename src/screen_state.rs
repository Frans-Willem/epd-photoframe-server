use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Offset, Utc};
use chrono_tz::Tz;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng, seq::SliceRandom};

use crate::config::Rotate;

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

/// Per-screen rotation state: the seed driving the current shuffle, the
/// navigation cursor, and a precomputed moment at which the next scheduled
/// rotation should fire.
pub struct ScreenState {
    seed: u64,
    cursor: i64,
    next_rotation: Option<DateTime<Utc>>,
}

impl ScreenState {
    pub fn fresh(rotate: Option<&Rotate>, tz: &Tz, now: DateTime<Utc>) -> Self {
        let next_rotation = rotate.and_then(|r| r.next_after(now, tz));
        if rotate.is_some() && next_rotation.is_none() {
            tracing::warn!("rotate schedule has no future triggers");
        }
        Self {
            seed: rand::rng().random(),
            cursor: 0,
            next_rotation,
        }
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn cursor(&self) -> i64 {
        self.cursor
    }

    pub fn next_rotation(&self) -> Option<DateTime<Utc>> {
        self.next_rotation
    }

    pub fn advance(&mut self, delta: i64) {
        self.cursor = self.cursor.wrapping_add(delta);
    }

    /// Reseed and reset the cursor if `now` has crossed the stored
    /// `next_rotation` moment. Advances `next_rotation` to the next trigger.
    pub fn maybe_rotate(&mut self, rotate: Option<&Rotate>, tz: &Tz, now: DateTime<Utc>) {
        let Some(next) = self.next_rotation else {
            return;
        };
        if now < next {
            return;
        }
        let old_seed = self.seed;
        self.seed = rand::rng().random();
        self.cursor = 0;
        self.next_rotation = rotate.and_then(|r| r.next_after(now, tz));
        tracing::info!(old_seed, new_seed = self.seed, next = ?self.next_rotation, "rotated screen");
    }
}

/// Fisher-Yates permutation of `[0..n)` seeded by `seed`, indexed by
/// `cursor.rem_euclid(n)`. Panics if `n == 0`.
pub fn resolve_index(seed: u64, cursor: i64, n: usize) -> usize {
    assert!(n > 0, "resolve_index called with empty album");
    let mut perm: Vec<usize> = (0..n).collect();
    let mut rng = StdRng::seed_from_u64(seed);
    perm.shuffle(&mut rng);
    perm[cursor.rem_euclid(n as i64) as usize]
}

/// Resolve an IANA timezone name (or the system default if None).
pub fn resolve_tz(name: Option<&str>) -> anyhow::Result<Tz> {
    let name = match name {
        Some(n) => n.to_string(),
        None => iana_time_zone::get_timezone().context("detecting system timezone")?,
    };
    name.parse::<Tz>()
        .map_err(|e| anyhow::anyhow!("unknown timezone `{name}`: {e}"))
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
    use std::str::FromStr;

    fn tz() -> Tz {
        "Europe/Amsterdam".parse().unwrap()
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
    fn cron_rotate_fires_after_next() {
        let rotate = Rotate::Cron(cron::Schedule::from_str("0 0 2 * * *").unwrap());
        let tz = tz();
        // Seed at 20 Apr 12:00 local
        let start = tz
            .with_ymd_and_hms(2026, 4, 20, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let mut s = ScreenState::fresh(Some(&rotate), &tz, start);
        assert!(s.next_rotation().is_some());
        let initial_next = s.next_rotation().unwrap();
        // Advance past 02:00 next day
        let later = tz
            .with_ymd_and_hms(2026, 4, 21, 3, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let seed_before = s.seed();
        s.advance(3);
        s.maybe_rotate(Some(&rotate), &tz, later);
        assert_eq!(s.cursor(), 0);
        assert_ne!(s.seed(), seed_before);
        assert!(s.next_rotation().unwrap() > initial_next);
    }

    #[test]
    fn cron_rotate_noop_before_next() {
        let rotate = Rotate::Cron(cron::Schedule::from_str("0 0 2 * * *").unwrap());
        let tz = tz();
        let start = tz
            .with_ymd_and_hms(2026, 4, 21, 3, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let mut s = ScreenState::fresh(Some(&rotate), &tz, start);
        s.advance(5);
        let snap = (s.seed(), s.cursor(), s.next_rotation());
        let now = start + chrono::Duration::hours(10);
        s.maybe_rotate(Some(&rotate), &tz, now);
        assert_eq!((s.seed(), s.cursor(), s.next_rotation()), snap);
    }

    #[test]
    fn no_schedule_means_no_rotation() {
        let tz = tz();
        let mut s = ScreenState::fresh(None, &tz, Utc::now());
        assert!(s.next_rotation().is_none());
        s.advance(7);
        s.maybe_rotate(None, &tz, Utc::now() + chrono::Duration::days(365));
        assert_eq!(s.cursor(), 7);
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
        let t = error_refresh_target(Duration::from_secs(3600), Duration::ZERO, None, now);
        assert_eq!(t, now + chrono::Duration::seconds(3600));
    }

    #[test]
    fn error_refresh_target_clamps_to_wake_target_when_sooner() {
        let now = Utc::now();
        // Next rotation in 10 min, wake_delay 5 min → cap is 15 min.
        let next_rotation = now + chrono::Duration::seconds(600);
        let t = error_refresh_target(
            Duration::from_secs(3600), // would-be 1 h
            Duration::from_secs(300),
            Some(next_rotation),
            now,
        );
        assert_eq!(t, next_rotation + chrono::Duration::seconds(300));
    }

    #[test]
    fn error_refresh_target_uses_base_when_wake_target_is_later() {
        let now = Utc::now();
        // Next rotation in 6 h → 1 h error_refresh wins.
        let next_rotation = now + chrono::Duration::hours(6);
        let t = error_refresh_target(
            Duration::from_secs(3600),
            Duration::ZERO,
            Some(next_rotation),
            now,
        );
        assert_eq!(t, now + chrono::Duration::seconds(3600));
    }
}
