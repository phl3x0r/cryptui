//! Number formatting for fixed-width tables.
//!
//! Contract prices span seven orders of magnitude (0.0179 to 85,632.70), so a
//! single fixed precision is either unreadable or uselessly coarse. Precision
//! adapts to magnitude instead, and large numbers get thousands separators.

/// Significant digits kept for prices.
const PRICE_SIGNIFICANT_DIGITS: i32 = 7;

/// Format a price, adapting precision to its magnitude.
pub fn price(value: f64) -> String {
    group(&format!(
        "{value:.decimals$}",
        decimals = price_decimals(value)
    ))
}

/// Format a contract quantity: up to four decimals, trailing zeros removed.
pub fn quantity(value: f64) -> String {
    group(&trim_decimals(&format!("{value:.4}")))
}

/// Format a monetary amount with two decimals.
pub fn money(value: f64) -> String {
    group(&format!("{value:.2}"))
}

/// Format a monetary amount with an explicit sign, for profit and loss.
pub fn signed_money(value: f64) -> String {
    signed_grouped(&format!("{value:.2}"), value)
}

/// Format a percentage with one decimal and an explicit sign.
pub fn percent(value: f64) -> String {
    format!("{}%", signed_grouped(&format!("{value:.1}"), value))
}

/// Format a percentage that is not a gain or loss, so it carries no sign.
///
/// Used for ratios such as the margin ratio, where `+2.4%` would read as profit.
pub fn percent_plain(value: f64) -> String {
    format!("{}%", group(&format!("{value:.1}")))
}

/// Format a millisecond timestamp as UTC `YYYY-MM-DD`.
pub fn date(milliseconds: i64) -> String {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    render(milliseconds, format)
}

/// Format a millisecond timestamp as UTC `MM-DD`.
pub fn day(milliseconds: i64) -> String {
    let format = time::macros::format_description!("[month]-[day]");
    render(milliseconds, format)
}

/// Format a millisecond timestamp as UTC `MM-DD HH:MM`.
pub fn timestamp(milliseconds: i64) -> String {
    let format = time::macros::format_description!("[month]-[day] [hour]:[minute]");
    render(milliseconds, format)
}

/// Format a timestamp, or an em dash when it cannot be represented.
fn render(milliseconds: i64, format: &[time::format_description::FormatItem<'_>]) -> String {
    time::OffsetDateTime::from_unix_timestamp(milliseconds / 1_000)
        .ok()
        .and_then(|value| value.format(format).ok())
        .unwrap_or_else(|| "—".to_owned())
}

/// Decimals to show for a price of this magnitude.
fn price_decimals(value: f64) -> usize {
    let magnitude = value.abs();
    if !magnitude.is_finite() || magnitude == 0.0 {
        return 2;
    }
    let integer_digits = magnitude.log10().floor() as i32 + 1;
    (PRICE_SIGNIFICANT_DIGITS - integer_digits).clamp(2, 8) as usize
}

/// Drop trailing zeros from a decimal string, but always keep a point and at
/// least two decimals.
fn trim_decimals(text: &str) -> String {
    let trimmed = text.trim_end_matches('0');
    let trimmed = trimmed.strip_suffix('.').unwrap_or(trimmed);
    let (integer, fraction) = match trimmed.split_once('.') {
        Some((integer, fraction)) => (integer, fraction),
        None => (trimmed, ""),
    };

    let mut result = String::with_capacity(trimmed.len() + 2);
    result.push_str(integer);
    result.push('.');
    result.push_str(fraction);
    for _ in fraction.len()..2 {
        result.push('0');
    }
    result
}

/// Add an explicit `+` when the value is not negative.
fn signed_grouped(text: &str, value: f64) -> String {
    let grouped = group(text);
    if value >= 0.0 {
        format!("+{grouped}")
    } else {
        grouped
    }
}

/// Insert thousands separators into the integer part.
fn group(text: &str) -> String {
    let (sign, unsigned) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text),
    };
    let (integer, fraction) = match unsigned.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (unsigned, None),
    };

    let mut grouped = String::with_capacity(text.len() + integer.len() / 3);
    for (index, digit) in integer.chars().enumerate() {
        if index > 0 && (integer.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }

    match fraction {
        Some(fraction) => format!("{sign}{grouped}.{fraction}"),
        None => format!("{sign}{grouped}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        date, day, money, percent, percent_plain, price, quantity, signed_money, timestamp,
    };

    #[test]
    fn large_prices_keep_two_decimals_and_separators() {
        assert_eq!(price(85_632.7), "85,632.70");
        assert_eq!(price(85_259.8), "85,259.80");
    }

    #[test]
    fn small_prices_keep_significant_digits() {
        assert_eq!(price(0.3194), "0.3194000");
        assert_eq!(price(0.0179), "0.01790000");
        assert_eq!(price(29.814576), "29.81458");
    }

    #[test]
    fn zero_is_not_mangled() {
        assert_eq!(price(0.0), "0.00");
        assert_eq!(money(0.0), "0.00");
    }

    #[test]
    fn quantities_trim_trailing_zeros_but_keep_two_decimals() {
        assert_eq!(quantity(212.0), "212.00");
        assert_eq!(quantity(81.2), "81.20");
        assert_eq!(quantity(0.123_456), "0.1235");
        assert_eq!(quantity(1_234.5), "1,234.50");
    }

    #[test]
    fn plain_percentages_carry_no_sign() {
        assert_eq!(percent_plain(2.414_2), "2.4%");
        assert_eq!(percent_plain(0.0), "0.0%");
    }

    #[test]
    fn dates_and_days_drop_the_time() {
        assert_eq!(date(1_790_726_400_000), "2026-09-30");
        assert_eq!(day(1_790_773_200_000), "09-30");
        assert_eq!(day(i64::MIN), "—");
    }

    #[test]
    fn timestamps_are_utc_and_stable() {
        // Verified against the live API: 1_790_773_200_000 ms is 12:30 UTC.
        assert_eq!(timestamp(1_790_773_200_000), "09-30 13:00");
        assert_eq!(timestamp(1_790_726_400_000), "09-30 00:00");
        assert_eq!(
            timestamp(i64::MIN),
            "—",
            "an impossible stamp degrades quietly"
        );
    }

    #[test]
    fn money_uses_two_decimals_and_separators() {
        assert_eq!(money(4_041.703_1), "4,041.70");
        assert_eq!(money(3_203.157_8), "3,203.16");
    }

    #[test]
    fn signed_values_always_show_a_sign() {
        assert_eq!(signed_money(141.25), "+141.25");
        assert_eq!(signed_money(-52.75), "-52.75");
        assert_eq!(signed_money(0.0), "+0.00");
        assert_eq!(percent(14.125), "+14.1%");
        assert_eq!(percent(-5.275), "-5.3%");
    }

    #[test]
    fn grouping_handles_boundaries() {
        assert_eq!(money(999.999), "1,000.00");
        assert_eq!(money(-1_234.5), "-1,234.50");
        assert_eq!(money(1_000_000.0), "1,000,000.00");
    }
}
