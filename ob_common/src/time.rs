//! Метки времени в формате ISO-8601 (UTC) без внешних зависимостей.
//!
//! Строки лексикографически сортируются в том же порядке, что и хронологически,
//! поэтому их можно класть в текстовую колонку `messages."time"` и сортировать
//! прямо в SQL.
use std::time::{SystemTime, UNIX_EPOCH};

/// Текущее время UTC как `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn now_iso() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    iso_from_millis(millis)
}

/// Текущее Unix-время в секундах.
pub fn unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Unix-время (секунды) -> `YYYY-MM-DDTHH:MM:SS.000Z`.
pub fn iso_from_unix(secs: i64) -> String {
    iso_from_millis(secs.saturating_mul(1000))
}

/// Unix-время (миллисекунды) -> `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn iso_from_millis(millis: i64) -> String {
    let secs = millis.div_euclid(1000);
    let ms = millis.rem_euclid(1000);

    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, mo, d) = civil_from_days(days);

    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{ms:03}Z")
}

/// Число дней от 1970-01-01 -> календарная дата (алгоритм Ховарда Хиннанта).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], март = 0
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_timestamps() {
        assert_eq!(iso_from_unix(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_unix(1), "1970-01-01T00:00:01.000Z");
        // 2001-09-09T01:46:40Z — классическая "миллиардная" секунда.
        assert_eq!(iso_from_unix(1_000_000_000), "2001-09-09T01:46:40.000Z");
        // 29 февраля високосного 2024-го.
        assert_eq!(iso_from_unix(1_709_208_896), "2024-02-29T12:14:56.000Z");
        assert_eq!(iso_from_millis(1_709_208_896_123), "2024-02-29T12:14:56.123Z");
    }

    #[test]
    fn ordering_is_lexicographic() {
        let a = iso_from_millis(1_709_208_896_000);
        let b = iso_from_millis(1_709_208_896_001);
        let c = iso_from_millis(1_709_295_296_000);
        assert!(a < b && b < c);
    }

    #[test]
    fn now_is_after_2020() {
        assert!(now_iso().as_str() > "2020-01-01T00:00:00.000Z");
    }
}
