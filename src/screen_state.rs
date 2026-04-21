use anyhow::Context;
use chrono::{DateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng, seq::SliceRandom};

/// Per-screen rotation state: which seed is driving the current day's photo
/// order, when it was last reseeded, and the navigation cursor.
pub struct ScreenState {
    seed: u64,
    seeded_at: DateTime<Utc>,
    cursor: i64,
}

impl ScreenState {
    pub fn fresh(now: DateTime<Utc>) -> Self {
        Self { seed: rand::rng().random(), seeded_at: now, cursor: 0 }
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn cursor(&self) -> i64 {
        self.cursor
    }

    pub fn advance(&mut self, delta: i64) {
        self.cursor = self.cursor.wrapping_add(delta);
    }

    /// Reseed and reset the cursor if `now` has crossed the most recent
    /// occurrence of `rotate_at` since `seeded_at`.
    pub fn maybe_rotate(
        &mut self,
        rotate_at: Option<NaiveTime>,
        tz: &Tz,
        now: DateTime<Utc>,
    ) {
        let Some(rotate_at) = rotate_at else { return };
        let Some(last) = last_rotate_before(rotate_at, tz, now) else { return };
        if self.seeded_at < last {
            let new_seed: u64 = rand::rng().random();
            tracing::info!(
                old_seed = self.seed,
                new_seed,
                "rotating screen seed"
            );
            self.seed = new_seed;
            self.seeded_at = now;
            self.cursor = 0;
        }
    }
}

/// Most recent occurrence of `rotate_at` strictly before `now`, in UTC.
fn last_rotate_before(
    rotate_at: NaiveTime,
    tz: &Tz,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let local_now = now.with_timezone(tz);
    let today = local_now.date_naive();
    let today_rotate = tz.from_local_datetime(&today.and_time(rotate_at)).earliest();
    if let Some(t) = today_rotate
        && t < local_now
    {
        return Some(t.with_timezone(&Utc));
    }
    let yesterday = today.pred_opt()?;
    let yest_rotate = tz
        .from_local_datetime(&yesterday.and_time(rotate_at))
        .earliest()?;
    Some(yest_rotate.with_timezone(&Utc))
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

pub fn parse_rotate_at(s: &str) -> anyhow::Result<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M"))
        .with_context(|| format!("invalid rotate_at `{s}` — expected HH:MM or HH:MM:SS"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn tz() -> Tz {
        "Europe/Amsterdam".parse().unwrap()
    }

    fn state(seed: u64, cursor: i64) -> ScreenState {
        ScreenState {
            seed,
            seeded_at: Utc::now(),
            cursor,
        }
    }

    #[test]
    fn resolve_index_is_a_permutation() {
        let seen: HashSet<usize> = (0..10).map(|c| resolve_index(42, c, 10)).collect();
        assert_eq!(seen.len(), 10);
    }

    #[test]
    fn resolve_index_wraps_negative_cursor() {
        let a = resolve_index(42, -1, 5);
        let b = resolve_index(42, 4, 5);
        assert_eq!(a, b);
    }

    #[test]
    fn resolve_index_wraps_past_n() {
        let a = resolve_index(42, 0, 5);
        let b = resolve_index(42, 5, 5);
        assert_eq!(a, b);
    }

    #[test]
    fn advance_wraps_on_overflow() {
        let mut s = state(1, i64::MAX);
        s.advance(1);
        assert_eq!(s.cursor(), i64::MIN);
    }

    #[test]
    fn rotation_fires_after_rotate_at() {
        let tz = tz();
        let rotate_at = NaiveTime::from_hms_opt(2, 0, 0).unwrap();
        let seeded = tz.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
        let mut s = ScreenState {
            seed: 1,
            seeded_at: seeded.with_timezone(&Utc),
            cursor: 5,
        };
        let now = tz
            .with_ymd_and_hms(2026, 4, 21, 3, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        s.maybe_rotate(Some(rotate_at), &tz, now);
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.seeded_at, now);
    }

    #[test]
    fn rotation_noop_when_seeded_after_last_rotate() {
        let tz = tz();
        let rotate_at = NaiveTime::from_hms_opt(2, 0, 0).unwrap();
        let seeded = tz.with_ymd_and_hms(2026, 4, 21, 3, 0, 0).unwrap();
        let seed_before = 1;
        let mut s = ScreenState {
            seed: seed_before,
            seeded_at: seeded.with_timezone(&Utc),
            cursor: 5,
        };
        let now = tz
            .with_ymd_and_hms(2026, 4, 21, 10, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        s.maybe_rotate(Some(rotate_at), &tz, now);
        assert_eq!(s.cursor(), 5);
        assert_eq!(s.seed(), seed_before);
    }

    #[test]
    fn rotation_noop_when_disabled() {
        let tz = tz();
        let seeded = tz.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let mut s = ScreenState {
            seed: 1,
            seeded_at: seeded.with_timezone(&Utc),
            cursor: 3,
        };
        let now = Utc::now();
        s.maybe_rotate(None, &tz, now);
        assert_eq!(s.cursor(), 3);
        assert_eq!(s.seed(), 1);
    }

    #[test]
    fn rotation_at_exact_rotate_at_uses_previous_day() {
        // At exactly rotate_at local time, "strictly before now" is yesterday.
        // seeded_at == yesterday's rotate means no rotation fires.
        let tz = tz();
        let rotate_at = NaiveTime::from_hms_opt(2, 0, 0).unwrap();
        let yesterday_rotate = tz.with_ymd_and_hms(2026, 4, 20, 2, 0, 0).unwrap();
        let mut s = ScreenState {
            seed: 1,
            seeded_at: yesterday_rotate.with_timezone(&Utc),
            cursor: 0,
        };
        let now = tz
            .with_ymd_and_hms(2026, 4, 21, 2, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        s.maybe_rotate(Some(rotate_at), &tz, now);
        assert_eq!(s.seed(), 1);
    }

    #[test]
    fn parse_rotate_at_accepts_hm_and_hms() {
        assert_eq!(
            parse_rotate_at("02:00").unwrap(),
            NaiveTime::from_hms_opt(2, 0, 0).unwrap()
        );
        assert_eq!(
            parse_rotate_at("14:30:45").unwrap(),
            NaiveTime::from_hms_opt(14, 30, 45).unwrap()
        );
        assert!(parse_rotate_at("noon").is_err());
    }
}
