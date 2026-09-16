//! Shared, exact display formatting for operating values.

use crate::model::{Money, UtcSeconds};

/// Formats signed integer cents as currency without changing the stored value.
pub(crate) fn money(value: Money) -> String {
    cents(i128::from(value.cents()))
}

/// Formats a signed cent amount with an explicit plus sign for gains.
pub(crate) fn signed_cents(value: i128) -> String {
    if value > 0 {
        format!("+{}", cents(value))
    } else {
        cents(value)
    }
}

fn cents(value: i128) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let absolute = value.unsigned_abs();
    format!("{sign}${}.{:02}", grouped(absolute / 100), absolute % 100)
}

fn grouped(value: u128) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (index, digit) in digits.chars().enumerate() {
        if index != 0 && (digits.len() - index) % 3 == 0 {
            result.push(',');
        }
        result.push(digit);
    }
    result
}

/// Formats metres in kilometres, retaining exact integer metres without
/// adding noisy zeroes to whole-kilometre labels.
pub(crate) fn distance(metres: u64) -> String {
    let whole = metres / 1_000;
    let remainder = metres % 1_000;
    if remainder == 0 {
        format!("{whole} km")
    } else {
        format!("{whole}.{remainder:03} km")
    }
}

/// Converts metres per second to km/h with one decimal place using integer
/// arithmetic (1 m/s = 3.6 km/h).
pub(crate) fn speed_kmh(metres_per_second: u64) -> String {
    let tenths = u128::from(metres_per_second).saturating_mul(36);
    format!("{}.{:01} km/h", tenths / 10, tenths % 10)
}

/// Formats a duration or remaining interval as hours/minutes/seconds.
pub(crate) fn duration(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m {seconds:02}s"),
        _ => format!("{hours}h {minutes:02}m {seconds:02}s"),
    }
}

/// Formats a UTC timestamp as a compact 24-hour clock value.
pub(crate) fn clock_time(timestamp: UtcSeconds) -> String {
    let seconds_of_day = timestamp.unix_seconds().rem_euclid(86_400);
    let hours = seconds_of_day / 3_600;
    let minutes = (seconds_of_day % 3_600) / 60;
    format!("{hours:02}:{minutes:02}")
}

#[cfg(test)]
mod tests {
    use crate::model::{Money, UtcSeconds};

    use super::{clock_time, distance, duration, money, signed_cents, speed_kmh};

    #[test]
    fn formats_exact_money_signs_and_large_values() {
        assert_eq!(money(Money::from_cents(0)), "$0.00");
        assert_eq!(money(Money::from_cents(1)), "$0.01");
        assert_eq!(money(Money::from_cents(-1)), "-$0.01");
        assert_eq!(money(Money::from_cents(12_345_678_901)), "$123,456,789.01");
        assert_eq!(signed_cents(12_345), "+$123.45");
        assert_eq!(signed_cents(-12_345), "-$123.45");
    }

    #[test]
    fn formats_operating_units_consistently() {
        assert_eq!(distance(10_050), "10.050 km");
        assert_eq!(distance(10_000), "10 km");
        assert_eq!(speed_kmh(100), "360.0 km/h");
        assert_eq!(duration(3_723), "1h 02m 03s");
        assert_eq!(duration(65), "1m 05s");
    }

    #[test]
    fn formats_utc_clock_time_without_date_noise() {
        assert_eq!(clock_time(UtcSeconds::from_unix_seconds(0)), "00:00");
        assert_eq!(
            clock_time(UtcSeconds::from_unix_seconds(13 * 3_600 + 21 * 60)),
            "13:21"
        );
    }
}
