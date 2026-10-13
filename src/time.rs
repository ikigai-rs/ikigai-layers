//! The kernel clock's reading as an `xsd:dateTime`, with no date library, and the bound on a
//! feed's wait.

use ikigai_core::Time;

/// The longest `wait=` a feed read may ask for, in seconds. The host refuses more, and a
/// participant clamps to it, so both sides link it from here (also `feed::MAX_WAIT_SECS`).
pub const MAX_WAIT_SECS: u64 = 60;

/// `time` as an `xsd:dateTime` in UTC with milliseconds: `2026-10-10T12:00:00.000Z`.
///
/// ```
/// use ikigai_core::Time;
/// use ikigai_layers::time::xsd_date_time;
///
/// assert_eq!(xsd_date_time(Time::from_millis(0)), "1970-01-01T00:00:00.000Z");
/// assert_eq!(
///     xsd_date_time(Time::from_millis(1_791_000_000_123)),
///     "2026-10-03T04:00:00.123Z"
/// );
/// ```
pub fn xsd_date_time(time: Time) -> String {
    let millis = time.as_millis();
    let secs = millis / 1000;
    let ms = millis % 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day): Howard Hinnant's
/// `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leap_day_and_year_end() {
        // 2024-02-29T00:00:00Z
        assert_eq!(
            xsd_date_time(Time::from_millis(1_709_164_800_000)),
            "2024-02-29T00:00:00.000Z"
        );
        // 2025-12-31T23:59:59.999Z
        assert_eq!(
            xsd_date_time(Time::from_millis(1_767_225_599_999)),
            "2025-12-31T23:59:59.999Z"
        );
    }
}
