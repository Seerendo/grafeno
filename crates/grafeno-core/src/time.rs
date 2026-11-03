use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// An instant in microseconds since the Unix epoch (UTC).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    pub const MIN: Self = Self(i64::MIN);
    pub const MAX: Self = Self(i64::MAX);

    pub fn now() -> Self {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self(elapsed.as_micros() as i64)
    }

    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    pub const fn from_secs(secs: i64) -> Self {
        Self(secs.saturating_mul(1_000_000))
    }

    pub const fn as_micros(self) -> i64 {
        self.0
    }

    /// The UTC calendar date as `YYYY-MM-DD` (proleptic Gregorian).
    pub fn date(self) -> String {
        // Howard Hinnant's `civil_from_days`.
        let days = self.0.div_euclid(86_400_000_000);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        format!("{year:04}-{month:02}-{day:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_dates() {
        assert_eq!(Timestamp::from_secs(0).date(), "1970-01-01");
        assert_eq!(Timestamp::from_secs(951_782_400).date(), "2000-02-29");
        assert_eq!(Timestamp::from_secs(1_793_664_000).date(), "2026-11-03");
        assert_eq!(Timestamp::from_secs(-1).date(), "1969-12-31");
    }
}
