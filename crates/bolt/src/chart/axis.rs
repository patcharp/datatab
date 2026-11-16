//! Tick generation and label formatting for chart axes.
//!
//! All formatting is pre-computed at chart-build time; `render()` only reads
//! the stored `TickLabel` strings.

use chrono::{DateTime, Datelike, Local, Offset, TimeZone, Utc};
use gpui::SharedString;

use crate::chart::format::datetime_in_zone;

/// A single axis tick with its data-space value and pre-formatted label.
#[derive(Debug, Clone, PartialEq)]
pub struct TickLabel {
    /// Data-space coordinate for positioning on the axis.
    pub value: f64,
    /// Pre-formatted display string.
    pub label: String,
}

// ---------------------------------------------------------------------------
// Numeric ticks
// ---------------------------------------------------------------------------

/// Generate nice numeric ticks for `[min, max]`.
///
/// Uses the "nice step" algorithm: round the step magnitude up to the nearest
/// value in `{1, 2, 5, 10}` per decade. A degenerate range (`min >= max`)
/// returns a single tick at `min`.
///
/// Neighbouring ticks never carry the same label: the formatter adds decimals
/// until they differ, and a tick that still repeats its predecessor is left
/// out, so the axis shows fewer ticks rather than the same number twice.
pub fn ticks_numeric(min: f64, max: f64, target_count: usize) -> Vec<TickLabel> {
    let target_count = target_count.max(1);

    if min >= max {
        return vec![TickLabel {
            value: min,
            label: format_numeric(min, 0),
        }];
    }

    let span = max - min;
    let raw_step = span / target_count as f64;
    let step = nice_step(raw_step);

    let first = (min / step).ceil() * step;
    let mut values = Vec::new();
    let mut t = first;

    while t <= max + step * 1e-9 {
        if t >= min - step * 1e-9 {
            values.push(t);
        }
        t += step;
    }

    label_numeric_ticks(values)
}

/// Extra decimals tried before a repeated label is dropped instead.
const MAX_EXTRA_TICK_DECIMALS: usize = 3;

/// Label numeric ticks so that no two neighbours read the same.
///
/// Rounding to a few significant figures can collapse neighbours: 615 000,
/// 615 200 and 615 400 all read `615K` at the default precision. Each pass adds
/// a decimal, and the first pass with distinct neighbours wins; when even the
/// last pass repeats, the repeated tick is dropped.
fn label_numeric_ticks(values: Vec<f64>) -> Vec<TickLabel> {
    let labelled = |extra_decimals: usize| -> Vec<TickLabel> {
        values
            .iter()
            .map(|&value| TickLabel {
                value,
                label: format_numeric(value, extra_decimals),
            })
            .collect()
    };

    let mut ticks = labelled(0);
    for extra_decimals in 1..=MAX_EXTRA_TICK_DECIMALS {
        if neighbouring_labels_differ(&ticks) {
            break;
        }
        ticks = labelled(extra_decimals);
    }

    ticks.dedup_by(|later, earlier| later.label == earlier.label);
    ticks
}

/// Whether every pair of neighbouring ticks carries a different label.
fn neighbouring_labels_differ(ticks: &[TickLabel]) -> bool {
    let mut labels = ticks.iter().map(|tick| tick.label.as_str());
    let mut previous = match labels.next() {
        Some(label) => label,
        None => return true,
    };

    for label in labels {
        if label == previous {
            return false;
        }
        previous = label;
    }

    true
}

/// Round `raw` up to the nearest nice step (1, 2, 5, or 10 times a power of 10).
fn nice_step(raw: f64) -> f64 {
    if raw <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f64.powf(raw.log10().floor());
    let fraction = raw / magnitude;

    let nice = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };

    nice * magnitude
}

/// Format a numeric value with up to 4 significant figures, plus
/// `extra_decimals` when a caller needs neighbouring ticks told apart.
///
/// Uses SI suffixes (`K`, `M`, `G`, `T`, `P`) for magnitudes `>= 1e3` so axis
/// labels read `2.5G` instead of `2.500e9`, the convention observability
/// tooling reads at a glance. Very small non-zero
/// magnitudes (`< 1e-3`) keep scientific notation since SI sub-unit suffixes
/// (`m`, `µ`, `n`) collide with axis-label glyphs.
fn format_numeric(v: f64, extra_decimals: usize) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    let abs = v.abs();

    if abs <= 1e-3 {
        return format!("{:.3e}", v);
    }

    if abs >= 1e3 {
        const SUFFIXES: &[(f64, &str)] =
            &[(1e15, "P"), (1e12, "T"), (1e9, "G"), (1e6, "M"), (1e3, "K")];

        for &(threshold, suffix) in SUFFIXES {
            if abs >= threshold {
                let scaled = v / threshold;
                let abs_scaled = scaled.abs();
                let decimals = if abs_scaled >= 100.0 {
                    0
                } else if abs_scaled >= 10.0 {
                    1
                } else {
                    2
                } + extra_decimals;
                let formatted = format!("{scaled:.decimals$}");
                let trimmed = formatted
                    .trim_end_matches('0')
                    .trim_end_matches('.')
                    .to_string();
                return format!("{trimmed}{suffix}");
            }
        }
    }

    // Up to 4 significant figures, then strip trailing zeros.
    let decimals = 4 + extra_decimals;
    let s = format!("{v:.decimals$}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

// ---------------------------------------------------------------------------
// Time ticks
// ---------------------------------------------------------------------------

/// Step values for the time tick ladder (milliseconds).
///
/// Must remain strictly ascending. Each entry is a "nice" human-readable
/// duration that `ticks_time` may select as a step boundary.
const NICE_TIME_STEPS_MS: &[f64] = &[
    1_000.0,            // 1s
    5_000.0,            // 5s
    15_000.0,           // 15s
    60_000.0,           // 1m
    5.0 * 60_000.0,     // 5m
    15.0 * 60_000.0,    // 15m
    3_600_000.0,        // 1h
    2.0 * 3_600_000.0,  // 2h
    3.0 * 3_600_000.0,  // 3h
    6.0 * 3_600_000.0,  // 6h
    12.0 * 3_600_000.0, // 12h
    86_400_000.0,       // 1d
    2.0 * 86_400_000.0, // 2d
    3.0 * 86_400_000.0, // 3d
    7.0 * 86_400_000.0, // 1w
];

/// Mean Gregorian month length, used only to pick a calendar step size.
const AVERAGE_MONTH_MS: f64 = 365.2425 / 12.0 * 86_400_000.0;

/// Calendar month steps tried before falling back to whole-year steps.
const CALENDAR_MONTH_STEPS: &[i64] = &[1, 3, 6];

/// Generate nice time ticks for `[min_ms, max_ms]` (milliseconds since Unix
/// epoch) in the machine's local zone.
pub fn ticks_time(min_ms: f64, max_ms: f64, target_count: usize) -> Vec<TickLabel> {
    ticks_time_in(&Local, min_ms, max_ms, target_count)
}

/// Generate nice time ticks for `[min_ms, max_ms]` (milliseconds since Unix
/// epoch) in `tz`.
///
/// Ticks land on whole boundaries of the local calendar, so an hourly tick
/// reads `19:00` on a UTC+7 machine and still lands on a whole local hour in a
/// zone whose offset is 30 or 45 minutes. Clock, day and month labels are all
/// written in `tz`, so the axis and the hover readout read one clock.
pub fn ticks_time_in<Z: TimeZone>(
    tz: &Z,
    min_ms: f64,
    max_ms: f64,
    target_count: usize,
) -> Vec<TickLabel> {
    let target_count = target_count.max(1);

    if min_ms >= max_ms {
        return vec![TickLabel {
            value: min_ms,
            label: format_time_ms_in(tz, min_ms, 1_000.0),
        }];
    }

    let span = max_ms - min_ms;
    let raw_step = span / target_count as f64;

    let Some(step) = NICE_TIME_STEPS_MS.iter().copied().find(|&s| s >= raw_step) else {
        return ticks_calendar_in(tz, min_ms, max_ms, raw_step);
    };

    let Some(min_offset_ms) = zone_offset_ms(tz, min_ms) else {
        return Vec::new();
    };
    let max_offset_ms = zone_offset_ms(tz, max_ms).unwrap_or(min_offset_ms);

    // Align in local wall-clock time: `wall = utc + offset` puts a local
    // midnight at a multiple of a day and a local whole hour at a multiple of
    // an hour, whatever the zone's offset is. Span both endpoint offsets so a
    // fall-back fold still visits wall times in the second occurrence.
    let mut wall = ((min_ms + min_offset_ms.min(max_offset_ms)) / step).ceil() * step;
    let last_wall = max_ms + max_offset_ms.max(min_offset_ms);
    let mut ticks = Vec::new();

    while wall <= last_wall + step * 1e-9 {
        for value in wall_to_utc_ms_all(tz, wall).into_iter().flatten() {
            if value >= min_ms - step * 1e-9 && value <= max_ms + step * 1e-9 {
                ticks.push(TickLabel {
                    value,
                    label: format_time_ms_in(tz, value, step),
                });
            }
        }
        wall += step;
    }

    ticks.sort_by(|left, right| left.value.total_cmp(&right.value));
    ticks.dedup_by(|later, earlier| (later.value - earlier.value).abs() < 1e-9);

    ticks
}

/// Generate ticks on calendar boundaries in `tz` for steps longer than a week.
///
/// Month and year lengths vary, so fixed-millisecond steps drift away from
/// month starts. Ticks land on the first day of a local month (steps of 1, 3
/// or 6 months, labelled `%Y-%m`) or of a local year (nice multiples of a
/// year, labelled `%Y`), aligned to multiples of the step so quarters start in
/// January.
fn ticks_calendar_in<Z: TimeZone>(
    tz: &Z,
    min_ms: f64,
    max_ms: f64,
    raw_step_ms: f64,
) -> Vec<TickLabel> {
    let raw_months = raw_step_ms / AVERAGE_MONTH_MS;

    let (step_months, format) = match CALENDAR_MONTH_STEPS
        .iter()
        .copied()
        .find(|&months| months as f64 >= raw_months)
    {
        Some(months) => (months, "%Y-%m"),
        None => {
            let step_years = nice_step(raw_months / 12.0).ceil().max(1.0) as i64;
            (step_years * 12, "%Y")
        }
    };

    let Some(min_offset_ms) = zone_offset_ms(tz, min_ms) else {
        return Vec::new();
    };
    let Some(min_local) =
        DateTime::<Utc>::from_timestamp_millis((min_ms + min_offset_ms).ceil() as i64)
    else {
        return Vec::new();
    };

    let mut month_index = i64::from(min_local.year()) * 12 + i64::from(min_local.month0());
    if month_start_in(tz, month_index).is_none_or(|start| start < min_ms) {
        month_index += 1;
    }

    let remainder = month_index.rem_euclid(step_months);
    if remainder != 0 {
        month_index += step_months - remainder;
    }

    let max_offset_ms = zone_offset_ms(tz, max_ms).unwrap_or(min_offset_ms);
    let Some(max_local) =
        DateTime::<Utc>::from_timestamp_millis((max_ms + max_offset_ms).floor() as i64)
    else {
        return Vec::new();
    };
    let max_month_index = i64::from(max_local.year()) * 12 + i64::from(max_local.month0());

    let mut ticks = Vec::new();

    while month_index <= max_month_index + 1 {
        if let Some(value) = month_start_in(tz, month_index) {
            if value > max_ms {
                break;
            }

            ticks.push(TickLabel {
                value,
                label: local_label(tz, value, format),
            });
        }
        month_index += step_months;
    }

    ticks
}

/// Local midnight on the first day of the month `index` months after year 0.
///
/// A wall time that a DST gap removes yields `None`; a repeated one resolves
/// to its first occurrence.
fn month_start_in<Z: TimeZone>(tz: &Z, index: i64) -> Option<f64> {
    let year = i32::try_from(index.div_euclid(12)).ok()?;
    let month = u32::try_from(index.rem_euclid(12) + 1).ok()?;
    let naive = chrono::NaiveDate::from_ymd_opt(year, month, 1)?.and_hms_opt(0, 0, 0)?;

    first_instant(tz, naive)
}

/// The UTC instant of the local wall time `naive` in `tz`.
///
/// A wall time that a DST gap removes yields `None`; a repeated one resolves
/// to its first occurrence.
fn first_instant<Z: TimeZone>(tz: &Z, naive: chrono::NaiveDateTime) -> Option<f64> {
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(datetime) => Some(datetime.timestamp_millis() as f64),
        chrono::LocalResult::Ambiguous(earliest, _) => Some(earliest.timestamp_millis() as f64),
        chrono::LocalResult::None => None,
    }
}

/// The UTC offset of `tz`, in milliseconds, at the instant `utc_ms`.
fn zone_offset_ms<Z: TimeZone>(tz: &Z, utc_ms: f64) -> Option<f64> {
    let utc = DateTime::<Utc>::from_timestamp_millis(utc_ms.round() as i64)?;
    let offset = tz.offset_from_utc_datetime(&utc.naive_utc()).fix();
    Some(f64::from(offset.local_minus_utc()) * 1_000.0)
}

/// All UTC instants whose local wall-clock time in `tz` is `wall_ms`.
///
/// A wall time removed by a DST gap yields `[None, None]`; a repeated wall
/// time during a fall-back fold yields both occurrences.
fn wall_to_utc_ms_all<Z: TimeZone>(tz: &Z, wall_ms: f64) -> [Option<f64>; 2] {
    let Some(naive) =
        DateTime::<Utc>::from_timestamp_millis(wall_ms.round() as i64).map(|dt| dt.naive_utc())
    else {
        return [None, None];
    };

    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(datetime) => [Some(datetime.timestamp_millis() as f64), None],
        chrono::LocalResult::Ambiguous(earliest, latest) => {
            let earliest_ms = earliest.timestamp_millis() as f64;
            let latest_ms = latest.timestamp_millis() as f64;
            if (latest_ms - earliest_ms).abs() < 1e-9 {
                [Some(earliest_ms), None]
            } else {
                [Some(earliest_ms), Some(latest_ms)]
            }
        }
        chrono::LocalResult::None => [None, None],
    }
}

/// Format a millisecond-since-epoch timestamp as a tick label in `tz`.
///
/// The format pattern is chosen based on the step magnitude so that labels are
/// as concise as possible while remaining unambiguous. Every pattern,
/// including the bare date and month labels, reads the zone's calendar.
fn format_time_ms_in<Z: TimeZone>(tz: &Z, ms: f64, step_ms: f64) -> String {
    let fmt = if step_ms < 60_000.0 {
        "%H:%M:%S"
    } else if step_ms < 3_600_000.0 {
        "%H:%M"
    } else if step_ms < 86_400_000.0 {
        "%m-%d %H:%M"
    } else {
        "%Y-%m-%d"
    };

    local_label(tz, ms, fmt)
}

/// Format `ms` in `tz` with `format`, falling back to the raw number when the
/// instant is out of chrono's range.
fn local_label<Z: TimeZone>(tz: &Z, ms: f64, format: &str) -> String {
    match datetime_in_zone(tz, ms) {
        Some(datetime) => datetime.format(format).to_string(),
        None => format!("{ms:.0}"),
    }
}

// ---------------------------------------------------------------------------
// Log1p ticks (Y axis in Log scale mode)
// ---------------------------------------------------------------------------

/// Generate readable Y-axis ticks for a log1p-scaled axis.
///
/// Input `min` and `max` are original data-space values (counts, etc.).
/// Tick *positions* are expressed in log1p space (`ln(y + 1)`) so the caller
/// can map them directly to screen coordinates using the same log1p bounds.
/// Tick *labels* show the rounded original-scale values so users read natural
/// numbers.
///
/// The tick strategy places a tick at each "decade" boundary in log1p space
/// (`e^k - 1` for integer `k`) that falls within `[min, max]`, padded with a
/// few intermediate values when the range spans less than two decades.
///
/// Returns at least one tick even for a degenerate range (`min >= max`).
/// Never produces non-finite tick positions.
pub fn ticks_log(min: f64, max: f64, target_count: usize) -> Vec<TickLabel> {
    let target_count = target_count.max(1);

    // Clamp to non-negative — log1p is only defined for y >= -1; counts are >= 0.
    let min = min.max(0.0);
    let max = max.max(min);

    if min >= max {
        let original = (min.exp() - 1.0).round().max(0.0);
        return vec![TickLabel {
            value: min,
            label: format_log_label(original),
        }];
    }

    // Work in log1p space: transform bounds.
    let log_min = (min + 1.0).ln();
    let log_max = (max + 1.0).ln();

    // Candidate original-scale counts at decade boundaries: 0, 1, 9, 99, 999 …
    // (values where e^k - 1 is an integer for k = 0, 1, 2, …).
    // We also interpolate linearly in log1p space when the range is narrow.
    let mut positions: Vec<f64> = Vec::new();

    // Decade boundaries whose log1p value falls inside [log_min, log_max].
    let k_start = log_min.floor() as i64;
    let k_end = log_max.ceil() as i64;

    for k in k_start..=k_end {
        // log1p position for decade k: k is already in log-space.
        let lv = k as f64;
        if lv >= log_min - 1e-9 && lv <= log_max + 1e-9 {
            positions.push(lv.clamp(log_min, log_max));
        }
    }

    // If fewer than target_count candidates, add linearly-spaced positions in
    // log1p space to reach a denser grid.
    if positions.len() < target_count {
        let step = (log_max - log_min) / target_count as f64;
        let mut t = log_min;
        while t <= log_max + step * 1e-9 {
            let candidate = t.clamp(log_min, log_max);
            // Only add if not already close to an existing position.
            let already_close = positions
                .iter()
                .any(|&p| (p - candidate).abs() < step * 0.4);
            if !already_close {
                positions.push(candidate);
            }
            t += step;
        }
        positions.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    }

    // Deduplicate very close positions (can happen at exact decade boundaries).
    positions.dedup_by(|a, b| (*a - *b).abs() < 1e-9);

    // Build TickLabel: position is in log1p space; label is the original-scale
    // integer count (inverse: original = e^lv - 1, rounded to nearest integer).
    positions
        .into_iter()
        .filter_map(|lv| {
            if !lv.is_finite() {
                return None;
            }
            let original = (lv.exp() - 1.0).round().max(0.0);
            Some(TickLabel {
                value: lv,
                label: format_log_label(original),
            })
        })
        .collect()
}

/// Format a count label for the log1p axis.
///
/// Uses integer formatting for whole numbers and up to 2 decimal places for
/// sub-unit values that can arise after inverse transform.
fn format_log_label(original: f64) -> String {
    if original == original.floor() && original.abs() < 1e12 {
        format!("{}", original as i64)
    } else {
        format!("{:.2}", original)
    }
}

// ---------------------------------------------------------------------------
// Categorical ticks (Bar / Scatter X axis — seam for next change)
// ---------------------------------------------------------------------------

/// Generate tick labels for a categorical (string) X axis.
///
/// Each distinct value in `values` becomes one tick; values are used verbatim
/// as labels. Position is the ordinal index (0, 1, 2, …).
///
/// This function is declared here as a seam for the Bar/Scatter change that
/// follows. The body returns an empty vec until the next change wires up the
/// full categorical-axis implementation.
#[allow(dead_code)]
pub fn ticks_categorical(values: &[SharedString]) -> Vec<TickLabel> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| TickLabel {
            value: i as f64,
            label: v.to_string(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Unit tests (strict TDD)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Timelike};

    #[test]
    fn ticks_numeric_span_1_to_100_returns_5_to_8_ticks() {
        let ticks = ticks_numeric(1.0, 100.0, 5);
        assert!(
            ticks.len() >= 5 && ticks.len() <= 8,
            "expected 5-8 ticks, got {}",
            ticks.len()
        );
        // All tick values should be within [1, 100].
        for t in &ticks {
            assert!(t.value >= 0.0 && t.value <= 110.0);
        }
    }

    #[test]
    fn ticks_numeric_span_0_001_uses_scientific_format() {
        let ticks = ticks_numeric(0.0001, 0.001, 5);
        assert!(!ticks.is_empty(), "should produce at least one tick");
        for t in &ticks {
            // Scientific notation for values < 1e-3.
            assert!(
                t.label.contains('e') || t.label == "0",
                "expected scientific format for {}, got {}",
                t.value,
                t.label
            );
        }
    }

    #[test]
    fn ticks_time_span_1_minute_uses_second_steps() {
        let base_ms = 0.0f64;
        let ticks = ticks_time_in(&fixed_offset(7, 0), base_ms, 60_000.0, 5);
        assert!(!ticks.is_empty());
        // Labels for <60s steps use HH:MM:SS format.
        for t in &ticks {
            // Should contain colons and be length 8 (HH:MM:SS).
            assert!(
                t.label.len() == 8 && t.label.matches(':').count() == 2,
                "expected HH:MM:SS, got {}",
                t.label
            );
        }
    }

    #[test]
    fn ticks_time_span_30_days_uses_day_steps() {
        let base_ms = 0.0f64;
        let end_ms = 30.0 * 86_400_000.0;
        let ticks = ticks_time_in(&fixed_offset(7, 0), base_ms, end_ms, 5);
        assert!(!ticks.is_empty());
        // Labels for >=1d steps use YYYY-MM-DD format.
        for t in &ticks {
            assert!(
                t.label.len() == 10 && t.label.matches('-').count() == 2,
                "expected YYYY-MM-DD, got {}",
                t.label
            );
        }
    }

    fn utc_ms(year: i32, month: u32, day: u32) -> f64 {
        Utc.with_ymd_and_hms(year, month, day, 0, 0, 0)
            .single()
            .expect("valid test date")
            .timestamp_millis() as f64
    }

    fn utc_ms_at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> f64 {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("valid test date")
            .timestamp_millis() as f64
    }

    /// A fixed UTC offset in whole hours and minutes, e.g. `fixed_offset(5, 30)`
    /// for UTC+05:30. Every test states its zone, so none reads or sets the
    /// process-wide timezone (#186).
    fn fixed_offset(hours: i32, minutes: i32) -> FixedOffset {
        let negative = hours < 0 || minutes < 0;
        let magnitude = hours.abs() * 3_600 + minutes.abs() * 60;
        FixedOffset::east_opt(if negative { -magnitude } else { magnitude })
            .expect("valid fixed zone offset")
    }

    fn tick_datetime(tick: &TickLabel) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp_millis(tick.value as i64).expect("tick within chrono range")
    }

    /// The wall-clock fields of `ms` in `tz`: year, month, day, hour, minute, second.
    fn zone_parts<Z: TimeZone>(tz: &Z, ms: f64) -> (i32, u32, u32, u32, u32, u32) {
        let datetime = datetime_in_zone(tz, ms).expect("tick within chrono range");
        (
            datetime.year(),
            datetime.month(),
            datetime.day(),
            datetime.hour(),
            datetime.minute(),
            datetime.second(),
        )
    }

    fn zone_label<Z: TimeZone>(tz: &Z, ms: f64, format: &str) -> String {
        datetime_in_zone(tz, ms)
            .expect("tick within chrono range")
            .format(format)
            .to_string()
    }

    /// A ten-minute window at 12:00 UTC reads 19:00..19:10 on UTC+7 — the
    /// owner's observation (#186) — and the clock labels carry no zone suffix.
    #[test]
    fn ticks_time_in_utc_plus_7_labels_ticks_in_that_zone() {
        let zone = fixed_offset(7, 0);
        let noon_utc = utc_ms_at(2025, 1, 2, 12, 0);
        let ticks = ticks_time_in(&zone, noon_utc, noon_utc + 600_000.0, 6);
        assert!(!ticks.is_empty(), "expected ticks for a ten-minute span");

        for tick in &ticks {
            assert_eq!(
                tick.label,
                zone_label(&zone, tick.value, "%H:%M"),
                "tick at {} ms is not labelled in the axis zone",
                tick.value
            );
            assert!(!tick.label.contains("UTC"), "clock label names a zone");
        }
        assert_eq!(ticks[0].label, "19:00");
    }

    /// Hourly ticks land on local whole hours on UTC+7, not on UTC hours.
    #[test]
    fn ticks_time_in_utc_plus_7_puts_hourly_ticks_on_local_hours() {
        let zone = fixed_offset(7, 0);
        let start = utc_ms_at(2025, 1, 2, 12, 0);
        let ticks = ticks_time_in(&zone, start, start + 6.0 * 3_600_000.0, 6);
        assert!(ticks.len() >= 5, "expected hourly ticks, got {ticks:?}");

        for tick in &ticks {
            let (_, _, _, _, minute, second) = zone_parts(&zone, tick.value);
            assert_eq!(
                (minute, second),
                (0, 0),
                "tick {} is not on a local whole hour",
                tick.label
            );
        }
        assert_eq!(ticks[0].value, start, "12:00 UTC is 19:00 on UTC+7");
    }

    /// A half-hour offset must still put hourly ticks on local whole hours:
    /// local hours sit at :30 past the UTC hour, not at :00.
    #[test]
    fn ticks_time_in_half_hour_offset_puts_hourly_ticks_on_local_hours() {
        let zone = fixed_offset(5, 30);
        let start = utc_ms_at(2025, 1, 2, 12, 0);
        let ticks = ticks_time_in(&zone, start, start + 6.0 * 3_600_000.0, 6);
        assert!(ticks.len() >= 5, "expected hourly ticks, got {ticks:?}");

        for tick in &ticks {
            let (_, _, _, _, minute, second) = zone_parts(&zone, tick.value);
            assert_eq!(
                (minute, second),
                (0, 0),
                "tick {} is not on a local whole hour",
                tick.label
            );
        }
        // 17:30 local is 12:00 UTC; the first local whole hour is 18:00 local.
        assert_eq!(tick_datetime(&ticks[0]).minute(), 30);
        assert_eq!(ticks[0].label, "01-02 18:00");
    }

    /// The quarter-hour offset (UTC+05:45) shifts local hours to :15 past UTC.
    #[test]
    fn ticks_time_in_quarter_hour_offset_puts_hourly_ticks_on_local_hours() {
        let zone = fixed_offset(5, 45);
        let start = utc_ms_at(2025, 1, 2, 12, 0);
        let ticks = ticks_time_in(&zone, start, start + 6.0 * 3_600_000.0, 6);
        assert!(ticks.len() >= 5, "expected hourly ticks, got {ticks:?}");

        for tick in &ticks {
            let (_, _, _, _, minute, second) = zone_parts(&zone, tick.value);
            assert_eq!(
                (minute, second),
                (0, 0),
                "tick {} is not on a local whole hour",
                tick.label
            );
        }
        assert_eq!(tick_datetime(&ticks[0]).minute(), 15);
        assert_eq!(ticks[0].label, "01-02 18:00");
    }

    /// An hourly tick on UTC+05:30 crosses the local date while UTC is still
    /// on the previous day, and it is labelled with the local date boundary.
    #[test]
    fn ticks_time_in_half_hour_offset_labels_a_tick_that_crosses_the_local_date() {
        let zone = fixed_offset(5, 30);
        let start = utc_ms_at(2025, 1, 1, 18, 0); // 23:30 local, 1 Jan
        let end = utc_ms_at(2025, 1, 2, 2, 0); // 07:30 local, 2 Jan
        let ticks = ticks_time_in(&zone, start, end, 6);
        assert!(
            !ticks.is_empty(),
            "expected ticks across the local midnight"
        );

        let midnight = ticks
            .iter()
            .find(|tick| zone_parts(&zone, tick.value).3 == 0)
            .expect("a tick at local midnight");
        assert_eq!(midnight.label, "01-02 00:00");
        assert_eq!(
            zone_parts(&zone, midnight.value).2,
            2,
            "local date is 2 Jan"
        );
        assert_eq!(
            tick_datetime(midnight).day(),
            1,
            "the same instant is still 1 Jan in UTC"
        );
    }

    /// Day ticks are local midnights, so a negative offset puts them at 08:00
    /// UTC and the label names the local date.
    #[test]
    fn ticks_time_in_negative_offset_puts_day_ticks_on_local_midnight() {
        let zone = fixed_offset(-8, 0);
        let start = utc_ms(2025, 1, 1);
        let ticks = ticks_time_in(&zone, start, start + 3.0 * 86_400_000.0, 3);
        assert!(ticks.len() >= 3, "expected day ticks, got {ticks:?}");

        for tick in &ticks {
            let (_, _, _, hour, minute, _) = zone_parts(&zone, tick.value);
            assert_eq!(
                (hour, minute),
                (0, 0),
                "tick {} is not at local midnight",
                tick.label
            );
            assert_eq!(tick.label, zone_label(&zone, tick.value, "%Y-%m-%d"));
        }
        assert_eq!(ticks[0].value, utc_ms_at(2025, 1, 1, 8, 0));
    }

    /// Month ticks are local month starts: on UTC+7 that is 17:00 UTC on the
    /// last day of the previous month, and the label names the local month.
    #[test]
    fn ticks_time_in_puts_month_ticks_on_local_month_starts() {
        let zone = fixed_offset(7, 0);
        let ticks = ticks_time_in(&zone, utc_ms(2024, 12, 1), utc_ms(2025, 12, 1), 12);
        assert!(ticks.len() >= 10, "expected monthly ticks, got {ticks:?}");

        for tick in &ticks {
            let (year, month, day, hour, minute, _) = zone_parts(&zone, tick.value);
            assert_eq!(
                (day, hour, minute),
                (1, 0, 0),
                "tick {} is not at a local month start",
                tick.label
            );
            assert_eq!(tick.label, format!("{year:04}-{month:02}"));
        }

        let march = ticks
            .iter()
            .find(|tick| tick.label == "2025-03")
            .expect("a March tick");
        assert_eq!(march.value, utc_ms_at(2025, 2, 28, 17, 0));
    }

    /// Year ticks are local year starts in the axis zone.
    #[test]
    fn ticks_time_in_puts_year_ticks_on_local_year_starts() {
        let zone = fixed_offset(-5, 0);
        let ticks = ticks_time_in(&zone, utc_ms(2015, 3, 10), utc_ms(2025, 8, 20), 6);
        assert!(
            ticks.len() >= 3,
            "expected several year ticks, got {ticks:?}"
        );

        for tick in &ticks {
            let (year, month, day, hour, minute, _) = zone_parts(&zone, tick.value);
            assert_eq!(
                (month, day, hour, minute),
                (1, 1, 0, 0),
                "tick {} is not at a local year start",
                tick.label
            );
            assert_eq!(tick.label, format!("{year:04}"));
        }
    }

    /// No clock, day or month tick says "UTC": the axis speaks one zone.
    #[test]
    fn ticks_time_labels_never_say_utc() {
        let start = utc_ms_at(2025, 1, 2, 12, 0);

        for zone in [
            fixed_offset(7, 0),
            fixed_offset(5, 30),
            fixed_offset(5, 45),
            fixed_offset(-8, 0),
            fixed_offset(0, 0),
        ] {
            for (span, target) in [
                (600_000.0, 6),
                (30.0 * 86_400_000.0, 6),
                (365.0 * 86_400_000.0, 12),
            ] {
                for tick in ticks_time_in(&zone, start, start + span, target) {
                    assert!(
                        !tick.label.contains("UTC"),
                        "tick {0} names a zone for {zone:?}",
                        tick.label
                    );
                }
            }
        }
    }

    /// `ticks_time` is the thin wrapper that ticks the machine's own zone.
    #[test]
    fn ticks_time_ticks_the_machine_zone() {
        let noon_utc = utc_ms_at(2025, 1, 2, 12, 0);
        assert_eq!(
            ticks_time(noon_utc, noon_utc + 600_000.0, 6),
            ticks_time_in(&Local, noon_utc, noon_utc + 600_000.0, 6)
        );
    }

    #[test]
    fn ticks_time_twelve_month_span_lands_on_month_starts() {
        let zone = fixed_offset(7, 0);
        let ticks = ticks_time_in(&zone, utc_ms(2025, 1, 1), utc_ms(2025, 12, 1), 6);
        assert!(ticks.len() >= 3, "expected several ticks, got {ticks:?}");

        for tick in &ticks {
            let (year, month, day, hour, minute, _) = zone_parts(&zone, tick.value);
            assert_eq!(
                (day, hour, minute),
                (1, 0, 0),
                "tick {} is not at the start of a month",
                tick.label
            );
            assert_eq!(tick.label, format!("{year:04}-{month:02}"));
        }
    }

    #[test]
    fn ticks_time_monthly_target_twelve_uses_one_month_steps() {
        let zone = fixed_offset(0, 0);
        let ticks = ticks_time_in(&zone, utc_ms(2025, 1, 1), utc_ms(2025, 12, 1), 12);
        let labels: Vec<&str> = ticks.iter().map(|tick| tick.label.as_str()).collect();

        assert_eq!(labels.first(), Some(&"2025-01"));
        assert_eq!(labels.last(), Some(&"2025-12"));
        assert_eq!(labels.len(), 12);
    }

    #[test]
    fn ticks_time_multi_year_span_uses_year_ticks() {
        let zone = fixed_offset(0, 0);
        let ticks = ticks_time_in(&zone, utc_ms(2015, 3, 10), utc_ms(2025, 8, 20), 6);
        assert!(ticks.len() >= 3, "expected several ticks, got {ticks:?}");

        for tick in &ticks {
            let (year, month, day, hour, minute, _) = zone_parts(&zone, tick.value);
            assert_eq!(
                (month, day, hour, minute),
                (1, 1, 0, 0),
                "tick {} is not at the start of a year",
                tick.label
            );
            assert_eq!(tick.label, format!("{year:04}"));
        }
    }

    /// Seam preservation: ensures `ticks_categorical` signature is stable.
    ///
    /// This test must remain green so that the Bar/Scatter change cannot land
    /// without satisfying the categorical-axis contract.
    #[test]
    fn ticks_categorical_returns_one_tick_per_value() {
        let values = vec![
            SharedString::from("alpha"),
            SharedString::from("beta"),
            SharedString::from("gamma"),
        ];
        let ticks = ticks_categorical(&values);
        assert_eq!(ticks.len(), 3, "one tick per input value");
        assert_eq!(ticks[0].value, 0.0);
        assert_eq!(ticks[0].label, "alpha");
        assert_eq!(ticks[1].value, 1.0);
        assert_eq!(ticks[1].label, "beta");
        assert_eq!(ticks[2].value, 2.0);
        assert_eq!(ticks[2].label, "gamma");
    }

    #[test]
    fn ticks_categorical_empty_input_returns_empty() {
        let ticks = ticks_categorical(&[]);
        assert!(ticks.is_empty());
    }

    // ---------------------------------------------------------------------------
    // ticks_log tests (TDD RED → GREEN for log1p Y-axis)
    // ---------------------------------------------------------------------------

    /// T-LOG-01: decade-ish ticks — positions are in log1p space.
    #[test]
    fn ticks_log_positions_are_in_log1p_space() {
        // Range 0..=99: log1p(0+1)=0, log1p(99+1)≈ln(100)≈4.6.
        let ticks = ticks_log(0.0, 99.0, 5);
        assert!(!ticks.is_empty(), "should produce ticks");

        // Every tick position must equal ln(original + 1) within tolerance.
        for t in &ticks {
            assert!(
                t.value.is_finite(),
                "tick position must be finite, got {}",
                t.value
            );
            // Position is in log1p space; it must be in [ln(1), ln(100)].
            assert!(
                t.value >= -1e-9,
                "position must be >= 0 (log1p of 0+1=0), got {}",
                t.value
            );
            assert!(
                t.value <= (100f64).ln() + 1e-9,
                "position must be <= ln(100), got {}",
                t.value
            );
        }
    }

    /// T-LOG-02: labels show original-scale integer counts.
    #[test]
    fn ticks_log_labels_are_original_scale_integers() {
        let ticks = ticks_log(0.0, 9999.0, 6);
        assert!(!ticks.is_empty());

        // Reconstruct original from position and compare to label.
        for t in &ticks {
            let reconstructed = (t.value.exp() - 1.0).round().max(0.0);
            // Label must be the integer representation of the reconstructed value.
            let expected_label = format!("{}", reconstructed as i64);
            assert_eq!(
                t.label, expected_label,
                "label '{}' should be original-scale integer '{}' (pos={})",
                t.label, expected_label, t.value
            );
        }
    }

    /// T-LOG-03: min == 0 is handled correctly (no NaN or -Inf).
    #[test]
    fn ticks_log_handles_min_zero() {
        let ticks = ticks_log(0.0, 100.0, 5);
        assert!(!ticks.is_empty());
        for t in &ticks {
            assert!(t.value.is_finite(), "position must be finite");
        }
        // First tick should represent count 0.
        let first = &ticks[0];
        assert_eq!(first.label, "0", "first tick should be label '0'");
    }

    /// T-LOG-04: degenerate range (min >= max) returns a single tick.
    #[test]
    fn ticks_log_degenerate_range_returns_single_tick() {
        let ticks = ticks_log(5.0, 5.0, 5);
        assert_eq!(
            ticks.len(),
            1,
            "degenerate range must produce exactly 1 tick"
        );
        assert!(ticks[0].value.is_finite());
    }

    #[test]
    fn ticks_time_labels_format_per_step_magnitude() {
        // 1-hour span → steps should be at minute or second resolution.
        let ticks = ticks_time_in(&fixed_offset(7, 0), 0.0, 3_600_000.0, 5);
        for t in &ticks {
            // HH:MM or HH:MM:SS — either is fine; must have at least one colon.
            assert!(
                t.label.contains(':'),
                "expected time label with colon, got {}",
                t.label
            );
        }
    }

    // ---------------------------------------------------------------------------
    // Tick density tests (issue #132)
    // ---------------------------------------------------------------------------

    /// A 3-week span with target 12 must produce at least 7 ticks.
    ///
    /// Before the ladder expansion the largest step ≤ raw_step was 1 week,
    /// yielding only 3 ticks. Adding 2d and 3d entries ensures a step ≤ 3d is
    /// chosen, producing ≥ 7 ticks across 21 days.
    #[test]
    fn ticks_time_3week_target12_yields_at_least_7() {
        let span_ms = 21.0 * 86_400_000.0; // 21 days
        let ticks = ticks_time_in(&fixed_offset(7, 0), 0.0, span_ms, 12);
        assert!(
            ticks.len() >= 7,
            "expected >= 7 ticks for 3-week span target 12, got {}",
            ticks.len()
        );
    }

    /// `ticks_numeric` with target 13 over [0, 25] must produce at least 6 ticks.
    ///
    /// This acts as a non-regression anchor: the nice-step algorithm already
    /// satisfies this for numeric axes, confirming the Y dynamic path will too.
    #[test]
    fn ticks_numeric_target13_range_0_25_yields_at_least_6() {
        let ticks = ticks_numeric(0.0, 25.0, 13);
        assert!(
            ticks.len() >= 6,
            "expected >= 6 ticks for [0, 25] target 13, got {}",
            ticks.len()
        );
    }

    /// A test-only timezone with one spring-forward gap and one fall-back fold:
    /// - Winter (+01:00) until 2025-03-31 23:00 UTC, then Summer (+02:00):
    ///   local 2025-04-01 00:00..01:00 is skipped (a month-start gap).
    /// - Summer (+02:00) until 2025-10-26 01:00 UTC, then Winter (+01:00):
    ///   local 2025-10-26 02:00..03:00 occurs twice (00:00..01:00 UTC and
    ///   01:00..02:00 UTC).
    #[derive(Clone, Copy, Debug)]
    struct TransitionZone;

    impl TransitionZone {
        fn winter() -> FixedOffset {
            FixedOffset::east_opt(3_600).expect("valid winter offset")
        }

        fn summer() -> FixedOffset {
            FixedOffset::east_opt(7_200).expect("valid summer offset")
        }

        fn spring_utc() -> chrono::NaiveDateTime {
            chrono::NaiveDate::from_ymd_opt(2025, 3, 31)
                .expect("valid date")
                .and_hms_opt(23, 0, 0)
                .expect("valid time")
        }

        fn fall_utc() -> chrono::NaiveDateTime {
            chrono::NaiveDate::from_ymd_opt(2025, 10, 26)
                .expect("valid date")
                .and_hms_opt(1, 0, 0)
                .expect("valid time")
        }

        fn is_summer_utc(utc: chrono::NaiveDateTime) -> bool {
            utc >= Self::spring_utc() && utc < Self::fall_utc()
        }
    }

    impl TimeZone for TransitionZone {
        type Offset = FixedOffset;

        fn from_offset(_offset: &Self::Offset) -> Self {
            Self
        }

        fn offset_from_local_date(
            &self,
            local: &chrono::NaiveDate,
        ) -> chrono::LocalResult<Self::Offset> {
            let noon = local.and_hms_opt(12, 0, 0).expect("valid noon");
            self.offset_from_local_datetime(&noon)
        }

        fn offset_from_local_datetime(
            &self,
            local: &chrono::NaiveDateTime,
        ) -> chrono::LocalResult<Self::Offset> {
            let winter = Self::winter();
            let summer = Self::summer();
            let utc_winter = *local - chrono::TimeDelta::hours(1);
            let utc_summer = *local - chrono::TimeDelta::hours(2);
            let valid_winter = !Self::is_summer_utc(utc_winter);
            let valid_summer = Self::is_summer_utc(utc_summer);

            match (valid_summer, valid_winter) {
                (true, true) => chrono::LocalResult::Ambiguous(summer, winter),
                (true, false) => chrono::LocalResult::Single(summer),
                (false, true) => chrono::LocalResult::Single(winter),
                (false, false) => chrono::LocalResult::None,
            }
        }

        fn offset_from_utc_date(&self, utc: &chrono::NaiveDate) -> Self::Offset {
            let noon = utc.and_hms_opt(12, 0, 0).expect("valid noon");
            self.offset_from_utc_datetime(&noon)
        }

        fn offset_from_utc_datetime(&self, utc: &chrono::NaiveDateTime) -> Self::Offset {
            if Self::is_summer_utc(*utc) {
                Self::summer()
            } else {
                Self::winter()
            }
        }
    }

    /// A chart wholly inside the second occurrence of a fall-back fold must
    /// still produce ticks in that occurrence instead of resolving every wall
    /// time to the earlier hour outside the range.
    #[test]
    fn ticks_time_in_fold_second_occurrence_produces_ticks() {
        let start = utc_ms_at(2025, 10, 26, 1, 10); // 02:10 (+01:00, 2nd pass)
        let end = utc_ms_at(2025, 10, 26, 1, 50); // 02:50 (+01:00, 2nd pass)
        let ticks = ticks_time_in(&TransitionZone, start, end, 4);

        let pairs: Vec<(f64, &str)> = ticks
            .iter()
            .map(|tick| (tick.value, tick.label.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (utc_ms_at(2025, 10, 26, 1, 15), "02:15"),
                (utc_ms_at(2025, 10, 26, 1, 30), "02:30"),
                (utc_ms_at(2025, 10, 26, 1, 45), "02:45"),
            ]
        );
    }

    /// A chart spanning both occurrences of a fall-back fold emits ticks from
    /// both hours in chronological order.
    #[test]
    fn ticks_time_in_fold_span_keeps_both_occurrences_in_order() {
        let start = utc_ms_at(2025, 10, 26, 0, 20); // 02:20 (+02:00, 1st pass)
        let end = utc_ms_at(2025, 10, 26, 1, 40); // 02:40 (+01:00, 2nd pass)
        let ticks = ticks_time_in(&TransitionZone, start, end, 6);

        let pairs: Vec<(f64, &str)> = ticks
            .iter()
            .map(|tick| (tick.value, tick.label.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (utc_ms_at(2025, 10, 26, 0, 30), "02:30"),
                (utc_ms_at(2025, 10, 26, 0, 45), "02:45"),
                (utc_ms_at(2025, 10, 26, 1, 0), "02:00"),
                (utc_ms_at(2025, 10, 26, 1, 15), "02:15"),
                (utc_ms_at(2025, 10, 26, 1, 30), "02:30"),
            ]
        );
    }

    /// A chart spanning a spring-forward gap skips the missing local hour and
    /// continues on valid wall times.
    #[test]
    fn ticks_time_in_spring_gap_skips_missing_hour() {
        let start = utc_ms_at(2025, 3, 31, 22, 20); // 23:20 (+01:00)
        let end = utc_ms_at(2025, 4, 1, 0, 40); // 02:40 (+02:00)
        let ticks = ticks_time_in(&TransitionZone, start, end, 10);

        let pairs: Vec<(f64, &str)> = ticks
            .iter()
            .map(|tick| (tick.value, tick.label.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (utc_ms_at(2025, 3, 31, 22, 30), "23:30"),
                (utc_ms_at(2025, 3, 31, 22, 45), "23:45"),
                (utc_ms_at(2025, 3, 31, 23, 0), "01:00"),
                (utc_ms_at(2025, 3, 31, 23, 15), "01:15"),
                (utc_ms_at(2025, 3, 31, 23, 30), "01:30"),
                (utc_ms_at(2025, 3, 31, 23, 45), "01:45"),
                (utc_ms_at(2025, 4, 1, 0, 0), "02:00"),
                (utc_ms_at(2025, 4, 1, 0, 15), "02:15"),
                (utc_ms_at(2025, 4, 1, 0, 30), "02:30"),
            ]
        );
    }

    /// When a month-start midnight falls in a DST gap, that boundary is skipped
    /// and later months in the range still get ticks.
    #[test]
    fn ticks_time_in_month_start_gap_skips_missing_month_and_continues() {
        let ticks = ticks_time_in(&TransitionZone, utc_ms(2025, 1, 1), utc_ms(2025, 12, 1), 12);
        let labels: Vec<&str> = ticks.iter().map(|tick| tick.label.as_str()).collect();

        assert_eq!(
            labels,
            vec![
                "2025-02", "2025-03", "2025-05", "2025-06", "2025-07", "2025-08", "2025-09",
                "2025-10", "2025-11", "2025-12",
            ]
        );
    }

    /// The owner's "Queries / sec" chart printed "615K" on three neighbouring
    /// ticks. Rounding may never make two ticks read the same (#186).
    #[test]
    fn ticks_numeric_neighbouring_labels_stay_distinct() {
        let ticks = ticks_numeric(615_000.0, 616_000.0, 5);
        assert!(ticks.len() >= 4, "expected several ticks, got {ticks:?}");

        let labels: Vec<&str> = ticks.iter().map(|tick| tick.label.as_str()).collect();
        for pair in labels.windows(2) {
            assert_ne!(
                pair[0], pair[1],
                "neighbouring ticks share a label: {labels:?}"
            );
        }
    }
}
