//! Readout value formatting for charts: the hover readout and toolbar labels.
//!
//! The data owner formats its cells; the chart formats the resolved numeric
//! values it plots — an X position on the axis and a Y readout value.

use chrono::{DateTime, FixedOffset, Local, Offset, TimeZone};

/// `ms` (Unix milliseconds) as a datetime in `tz`, expressed at a fixed offset.
///
/// The zone's own offset type stays out of the return type, so callers can
/// format the value without naming it.
pub fn datetime_in_zone<Z: TimeZone>(tz: &Z, ms: f64) -> Option<DateTime<FixedOffset>> {
    if !ms.is_finite() {
        return None;
    }
    let utc = DateTime::from_timestamp_millis(ms.round() as i64)?;
    let offset = tz.offset_from_utc_datetime(&utc.naive_utc()).fix();
    Some(utc.with_timezone(&offset))
}

/// Format an X readout value in the machine's local zone: wall-clock time for
/// a time axis, otherwise a fixed three-decimal number.
///
/// A time value arrives as Unix milliseconds; out-of-range values fall back
/// to the numeric form rather than failing the readout.
pub fn format_x_value(x: f64, is_time: bool) -> String {
    format_x_value_in(&Local, x, is_time)
}

/// Format an X readout value in `tz`: wall-clock time for a time axis,
/// otherwise a fixed three-decimal number.
pub fn format_x_value_in<Z: TimeZone>(tz: &Z, x: f64, is_time: bool) -> String {
    if is_time && let Some(dt) = datetime_in_zone(tz, x) {
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        format!("{:.3}", x)
    }
}

/// Format a Y readout value for tooltips and hover overlays.
///
/// Magnitudes `>= 1e3` collapse to SI suffixes (`K`, `M`, `G`, `T`, `P`) so
/// the readout matches the axis-tick formatter (`2.5G`, not `2.500e9`). Very
/// small non-zero magnitudes (`< 1e-3`) keep scientific notation — SI
/// sub-unit suffixes would clash with axis glyphs. Other values keep up to
/// three decimals without trailing zeros, so integer values read `21`, not
/// `21.000`.
pub fn format_y_value(y: f64) -> String {
    if y == 0.0 {
        return "0".to_string();
    }
    let abs = y.abs();

    if abs < 1e-3 {
        return format!("{:.3e}", y);
    }

    if abs >= 1e3 {
        const SUFFIXES: &[(f64, &str)] =
            &[(1e15, "P"), (1e12, "T"), (1e9, "G"), (1e6, "M"), (1e3, "K")];

        for &(threshold, suffix) in SUFFIXES {
            if abs >= threshold {
                let scaled = y / threshold;
                let abs_scaled = scaled.abs();
                let formatted = if abs_scaled >= 100.0 {
                    format!("{:.0}", scaled)
                } else if abs_scaled >= 10.0 {
                    format!("{:.1}", scaled)
                } else {
                    format!("{:.2}", scaled)
                };
                let trimmed = formatted
                    .trim_end_matches('0')
                    .trim_end_matches('.')
                    .to_string();
                return format!("{trimmed}{suffix}");
            }
        }
    }

    format!("{:.3}", y)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_in_zone_shifts_the_clock_by_the_offset() {
        let plus_seven = chrono::FixedOffset::east_opt(7 * 3_600).expect("valid offset");
        let halfway = chrono::FixedOffset::east_opt(5 * 3_600 + 30 * 60).expect("valid offset");

        assert_eq!(
            datetime_in_zone(&plus_seven, 0.0)
                .expect("epoch in range")
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            "1970-01-01 07:00"
        );
        assert_eq!(
            datetime_in_zone(&halfway, 0.0)
                .expect("epoch in range")
                .format("%H:%M")
                .to_string(),
            "05:30"
        );
    }

    fn fixed_offset(hours: i32, minutes: i32) -> FixedOffset {
        let negative = hours < 0 || minutes < 0;
        let magnitude = hours.abs() * 3_600 + minutes.abs() * 60;
        FixedOffset::east_opt(if negative { -magnitude } else { magnitude })
            .expect("valid fixed zone offset")
    }

    #[test]
    fn format_x_value_time_formats_unix_ms() {
        let utc = fixed_offset(0, 0);
        let s = format_x_value_in(&utc, 0.0, true);
        assert_eq!(s, "1970-01-01 00:00:00");
    }

    #[test]
    fn format_x_value_in_uses_the_injected_zone() {
        assert_eq!(
            format_x_value_in(&fixed_offset(7, 0), 0.0, true),
            "1970-01-01 07:00:00"
        );
        assert_eq!(
            format_x_value_in(&fixed_offset(5, 30), 0.0, true),
            "1970-01-01 05:30:00"
        );
        assert_eq!(
            format_x_value_in(&fixed_offset(-8, 0), 0.0, true),
            "1969-12-31 16:00:00"
        );
        assert_eq!(
            format_x_value_in(&fixed_offset(7, 0), 42.5, false),
            "42.500"
        );
    }

    #[test]
    fn format_x_value_wraps_the_machine_zone() {
        assert_eq!(
            format_x_value(0.0, true),
            format_x_value_in(&Local, 0.0, true)
        );
    }

    #[test]
    fn format_y_value_uses_si_suffix_for_large_magnitudes() {
        // SI suffixes for values >= 1e3
        assert_eq!(format_y_value(1234.0), "1.23K");
        assert_eq!(format_y_value(2_500_000_000.0), "2.5G");
        assert_eq!(format_y_value(98_790_000_000.0), "98.8G");
        assert_eq!(format_y_value(4.936e10), "49.4G");

        // Scientific notation kept only for very small magnitudes.
        assert!(format_y_value(0.0001).contains('e'));

        // Pass-through formatting for the readable range.
        assert_eq!(format_y_value(0.0), "0");
        assert_eq!(format_y_value(1.5), "1.5");
    }

    #[test]
    fn format_y_value_drops_trailing_zeros_below_one_thousand() {
        assert_eq!(format_y_value(21.0), "21");
        assert_eq!(format_y_value(-7.0), "-7");
        assert_eq!(format_y_value(0.125), "0.125");
        assert_eq!(format_y_value(12.25), "12.25");
        assert_eq!(format_y_value(999.0), "999");
    }
}
