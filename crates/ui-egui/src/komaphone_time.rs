//! Viewer-local date/time display formatting for Komaphone UI surfaces (QOL-015).
//!
//! Display formatting only: stored, synced, ordered, and archived timestamps stay untouched.
//! Uses `chrono` for all calendar and timezone conversions, respects the viewer PC's system
//! timezone (including per-instant daylight saving offsets) and 12-/24-hour clock preference
//! where available, and falls back explicitly to UTC on platforms without system timezone data.

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Timelike, Utc};

const MAX_TIMESTAMP_BYTES: usize = 128;
pub const UNAVAILABLE_LABEL: &str = "Date unavailable";

/// Display text for compact cards/rows plus an optional tooltip retaining exact timestamp details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattedTimestamp {
    pub display: String,
    pub tooltip: Option<String>,
}

/// Viewer 12-hour vs 24-hour clock display convention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockFormat {
    Hour12,
    Hour24,
}

/// Resolved local instant and clear timezone label for display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedZone {
    pub offset: FixedOffset,
    pub label: Option<&'static str>,
    pub is_utc_fallback: bool,
}

/// Format an RFC 3339 timestamp string (or ISO `YYYY-MM-DD` date) for user-facing display
/// in the viewing PC's timezone. Invalid or missing values return [`UNAVAILABLE_LABEL`].
pub fn format_compact(raw: &str) -> FormattedTimestamp {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_TIMESTAMP_BYTES {
        return FormattedTimestamp { display: UNAVAILABLE_LABEL.to_owned(), tooltip: None };
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(trimmed) {
        return format_utc_instant(parsed.with_timezone(&Utc), Some(trimmed));
    }
    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let display = format_date_only(date);
        return FormattedTimestamp { display, tooltip: Some(format!("Date: {trimmed}")) };
    }
    FormattedTimestamp { display: UNAVAILABLE_LABEL.to_owned(), tooltip: None }
}

/// Format a positive Unix-epoch millisecond timestamp (e.g. `TrashedCanvas::deleted_at_ms`)
/// for user-facing display in the viewing PC's timezone.
pub fn format_unix_ms(ms: u64) -> FormattedTimestamp {
    if ms == 0 {
        return FormattedTimestamp { display: UNAVAILABLE_LABEL.to_owned(), tooltip: None };
    }
    let Ok(ms_i64) = i64::try_from(ms) else {
        return FormattedTimestamp { display: UNAVAILABLE_LABEL.to_owned(), tooltip: None };
    };
    let Some(instant) = DateTime::<Utc>::from_timestamp_millis(ms_i64) else {
        return FormattedTimestamp { display: UNAVAILABLE_LABEL.to_owned(), tooltip: None };
    };
    let exact = instant.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    format_utc_instant(instant, Some(&exact))
}

fn format_utc_instant(instant: DateTime<Utc>, original_raw: Option<&str>) -> FormattedTimestamp {
    let (zone, clock) = resolve_viewer(instant);
    let local = instant.with_timezone(&zone.offset);
    let zone_text = zone_label_text(zone);
    let date_part = format_date_only(local.date_naive());
    let compact_time = format_clock_time(local.hour(), local.minute(), None, clock);
    let detailed_time = format_clock_time(local.hour(), local.minute(), Some(local.second()), clock);
    let display = format!("{date_part}, {compact_time} {zone_text}");
    let exact_utc = original_raw.map_or_else(|| instant.to_rfc3339_opts(chrono::SecondsFormat::Secs, true), ToOwned::to_owned);
    let tooltip = if zone.is_utc_fallback {
        format!("{date_part}, {detailed_time} {zone_text} (system timezone unavailable · {exact_utc})")
    } else {
        format!("{date_part}, {detailed_time} {zone_text} (Exact: {exact_utc})")
    };
    FormattedTimestamp { display, tooltip: Some(tooltip) }
}

fn format_date_only(date: NaiveDate) -> String {
    format!("{} {}, {}", short_month(date.month()), date.day(), date.year())
}

fn short_month(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "?",
    }
}

fn format_clock_time(hour24: u32, minute: u32, second: Option<u32>, clock: ClockFormat) -> String {
    match clock {
        ClockFormat::Hour12 => {
            let (pm, hour12) = match hour24 {
                0 => (false, 12),
                1..=11 => (false, hour24),
                12 => (true, 12),
                _ => (true, hour24.saturating_sub(12)),
            };
            let meridiem = if pm { "PM" } else { "AM" };
            match second {
                Some(sec) => format!("{hour12}:{minute:02}:{sec:02} {meridiem}"),
                None => format!("{hour12}:{minute:02} {meridiem}"),
            }
        }
        ClockFormat::Hour24 => match second {
            Some(sec) => format!("{hour24:02}:{minute:02}:{sec:02}"),
            None => format!("{hour24:02}:{minute:02}"),
        },
    }
}

fn zone_label_text(zone: ResolvedZone) -> String {
    if zone.is_utc_fallback {
        return "UTC".to_owned();
    }
    if let Some(label) = zone.label {
        return label.to_owned();
    }
    format_utc_offset(zone.offset.local_minus_utc())
}

/// Unambiguous numeric UTC offset label (`UTC`, `UTC-8`, `UTC+5:30`).
pub fn format_utc_offset(offset_secs: i32) -> String {
    if offset_secs == 0 {
        return "UTC".to_owned();
    }
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let total_mins = offset_secs.unsigned_abs() / 60;
    let hours = total_mins / 60;
    let mins = total_mins % 60;
    if mins == 0 { format!("UTC{sign}{hours}") } else { format!("UTC{sign}{hours}:{mins:02}") }
}

/// Map a Windows timezone key or IANA zone identifier AND the verified per-instant UTC offset
/// (in seconds) to an unambiguous short zone abbreviation. Always verifies the offset matches so
/// winter/summer DST boundaries or timezone changes can never produce a mismatched abbreviation.
#[cfg_attr(all(not(test), target_arch = "wasm32"), allow(dead_code))]
fn abbreviation_for(zone_id: &str, offset_secs: i32) -> Option<&'static str> {
    match (zone_id.trim(), offset_secs) {
        ("Pacific Standard Time" | "America/Los_Angeles" | "America/Vancouver" | "America/Tijuana", -28_800) => Some("PST"),
        ("Pacific Standard Time" | "America/Los_Angeles" | "America/Vancouver" | "America/Tijuana", -25_200) => Some("PDT"),
        ("Mountain Standard Time" | "US Mountain Standard Time" | "America/Denver" | "America/Phoenix" | "America/Edmonton", -25_200) => Some("MST"),
        ("Mountain Standard Time" | "America/Denver" | "America/Edmonton", -21_600) => Some("MDT"),
        ("Central Standard Time" | "America/Chicago" | "America/Winnipeg" | "America/Mexico_City", -21_600) => Some("CST"),
        ("Central Standard Time" | "America/Chicago" | "America/Winnipeg", -18_000) => Some("CDT"),
        ("Eastern Standard Time" | "US Eastern Standard Time" | "America/New_York" | "America/Toronto" | "America/Detroit", -18_000) => Some("EST"),
        ("Eastern Standard Time" | "US Eastern Standard Time" | "America/New_York" | "America/Toronto" | "America/Detroit", -14_400) => Some("EDT"),
        ("Alaskan Standard Time" | "America/Anchorage", -32_400) => Some("AKST"),
        ("Alaskan Standard Time" | "America/Anchorage", -28_800) => Some("AKDT"),
        ("Hawaiian Standard Time" | "Pacific/Honolulu", -36_000) => Some("HST"),
        ("Atlantic Standard Time" | "America/Halifax", -14_400) => Some("AST"),
        ("Atlantic Standard Time" | "America/Halifax", -10_800) => Some("ADT"),
        ("Newfoundland Standard Time" | "America/St_Johns", -12_600) => Some("NST"),
        ("Newfoundland Standard Time" | "America/St_Johns", -9_000) => Some("NDT"),
        ("GMT Standard Time" | "Europe/London" | "Europe/Dublin", 0) => Some("GMT"),
        ("GMT Standard Time" | "Europe/London", 3_600) => Some("BST"),
        ("UTC" | "Etc/UTC" | "Etc/GMT" | "Coordinated Universal Time", 0) => Some("UTC"),
        (
            "W. Europe Standard Time"
            | "Central Europe Standard Time"
            | "Central European Standard Time"
            | "Romance Standard Time"
            | "Europe/Berlin"
            | "Europe/Paris"
            | "Europe/Rome"
            | "Europe/Madrid"
            | "Europe/Amsterdam"
            | "Europe/Brussels"
            | "Europe/Prague"
            | "Europe/Warsaw"
            | "Europe/Vienna"
            | "Europe/Zurich"
            | "Europe/Stockholm"
            | "Europe/Oslo"
            | "Europe/Copenhagen",
            3_600,
        ) => Some("CET"),
        (
            "W. Europe Standard Time"
            | "Central Europe Standard Time"
            | "Central European Standard Time"
            | "Romance Standard Time"
            | "Europe/Berlin"
            | "Europe/Paris"
            | "Europe/Rome"
            | "Europe/Madrid"
            | "Europe/Amsterdam"
            | "Europe/Brussels"
            | "Europe/Prague"
            | "Europe/Warsaw"
            | "Europe/Vienna"
            | "Europe/Zurich"
            | "Europe/Stockholm"
            | "Europe/Oslo"
            | "Europe/Copenhagen",
            7_200,
        ) => Some("CEST"),
        (
            "E. Europe Standard Time"
            | "FLE Standard Time"
            | "GTB Standard Time"
            | "Europe/Helsinki"
            | "Europe/Athens"
            | "Europe/Kyiv"
            | "Europe/Bucharest"
            | "Europe/Sofia",
            7_200,
        ) => Some("EET"),
        (
            "E. Europe Standard Time"
            | "FLE Standard Time"
            | "GTB Standard Time"
            | "Europe/Helsinki"
            | "Europe/Athens"
            | "Europe/Kyiv"
            | "Europe/Bucharest"
            | "Europe/Sofia",
            10_800,
        ) => Some("EEST"),
        ("Tokyo Standard Time" | "Asia/Tokyo", 32_400) => Some("JST"),
        ("Korea Standard Time" | "Asia/Seoul", 32_400) => Some("KST"),
        ("India Standard Time" | "Asia/Kolkata" | "Asia/Calcutta", 19_800) => Some("IST"),
        ("AUS Eastern Standard Time" | "E. Australia Standard Time" | "Australia/Sydney" | "Australia/Melbourne" | "Australia/Brisbane", 36_000) => {
            Some("AEST")
        }
        ("AUS Eastern Standard Time" | "Australia/Sydney" | "Australia/Melbourne", 39_600) => Some("AEDT"),
        ("New Zealand Standard Time" | "Pacific/Auckland", 43_200) => Some("NZST"),
        ("New Zealand Standard Time" | "Pacific/Auckland", 46_800) => Some("NZDT"),
        _ => None,
    }
}

/// Detect whether a Windows `sShortTime` format string specifies 24-hour (`H`) or 12-hour (`h`/`t`).
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn clock_from_windows_pattern(pattern: &str) -> Option<ClockFormat> {
    let mut in_quotes = false;
    let mut saw_12 = false;
    for ch in pattern.chars() {
        if ch == '\'' {
            in_quotes = !in_quotes;
            continue;
        }
        if in_quotes {
            continue;
        }
        if ch == 'H' {
            return Some(ClockFormat::Hour24);
        }
        if matches!(ch, 'h' | 't') {
            saw_12 = true;
        }
    }
    saw_12.then_some(ClockFormat::Hour12)
}

#[cfg_attr(not(any(test, not(target_arch = "wasm32"))), allow(dead_code))]
fn clock_from_locale_tag(tag: &str) -> ClockFormat {
    let lower = tag.trim().to_ascii_lowercase().replace('_', "-");
    let base = lower.split(['.', '@']).next().unwrap_or("");
    let is_12h = matches!(base, "en" | "en-us" | "en-ca" | "en-au" | "en-nz" | "en-ph" | "en-in" | "es-us" | "es-mx" | "ar-eg" | "ar-sa");
    if is_12h { ClockFormat::Hour12 } else { ClockFormat::Hour24 }
}

#[cfg(target_os = "windows")]
fn detect_windows_clock_format() -> Option<ClockFormat> {
    use winsafe::{self as w, co};
    if let Ok(w::RegistryValue::Sz(short_time)) =
        w::HKEY::CURRENT_USER.RegGetValue(Some("Control Panel\\International"), Some("sShortTime"), co::RRF::RT_REG_SZ)
        && let Some(clock) = clock_from_windows_pattern(&short_time)
    {
        return Some(clock);
    }
    if let Ok(w::RegistryValue::Sz(i_time)) = w::HKEY::CURRENT_USER.RegGetValue(Some("Control Panel\\International"), Some("iTime"), co::RRF::RT_REG_SZ) {
        match i_time.trim() {
            "1" => return Some(ClockFormat::Hour24),
            "0" => return Some(ClockFormat::Hour12),
            _ => {}
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn detect_windows_zone_key() -> Option<String> {
    use winsafe::{self as w, co};
    match w::HKEY::LOCAL_MACHINE.RegGetValue(Some("SYSTEM\\CurrentControlSet\\Control\\TimeZoneInformation"), Some("TimeZoneKeyName"), co::RRF::RT_REG_SZ) {
        Ok(w::RegistryValue::Sz(name)) if !name.trim().is_empty() => Some(name.trim().to_owned()),
        _ => None,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn system_clock_format() -> ClockFormat {
    static CLOCK: std::sync::OnceLock<ClockFormat> = std::sync::OnceLock::new();
    *CLOCK.get_or_init(|| {
        #[cfg(target_os = "windows")]
        if let Some(clock) = detect_windows_clock_format() {
            return clock;
        }
        let first_tag = std::panic::catch_unwind(|| sys_locale::get_locales().next()).ok().flatten();
        first_tag.as_deref().map_or(ClockFormat::Hour12, clock_from_locale_tag)
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn system_zone_id() -> Option<&'static str> {
    static ZONE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ZONE.get_or_init(|| {
        #[cfg(target_os = "windows")]
        if let Some(key) = detect_windows_zone_key() {
            return Some(key);
        }
        std::panic::catch_unwind(iana_time_zone::get_timezone).ok().and_then(Result::ok)
    })
    .as_deref()
}

#[cfg(not(target_arch = "wasm32"))]
fn resolve_system_viewer(instant: DateTime<Utc>) -> (ResolvedZone, ClockFormat) {
    let local = instant.with_timezone(&chrono::Local);
    let offset = *local.offset();
    let label = system_zone_id().and_then(|id| abbreviation_for(id, offset.local_minus_utc()));
    (ResolvedZone { offset, label, is_utc_fallback: false }, system_clock_format())
}

#[cfg(target_arch = "wasm32")]
fn resolve_system_viewer(_instant: DateTime<Utc>) -> (ResolvedZone, ClockFormat) {
    // UTC+0 is always valid; `FixedOffset::east_opt(0)` cannot fail.
    #[allow(clippy::expect_used)]
    let utc = FixedOffset::east_opt(0).expect("UTC offset 0 is always valid");
    (ResolvedZone { offset: utc, label: Some("UTC"), is_utc_fallback: true }, ClockFormat::Hour12)
}

#[cfg(not(test))]
fn resolve_viewer(instant: DateTime<Utc>) -> (ResolvedZone, ClockFormat) {
    resolve_system_viewer(instant)
}

#[cfg(test)]
pub(crate) type ViewerOverrideFn = fn(DateTime<Utc>) -> (ResolvedZone, ClockFormat);

#[cfg(test)]
thread_local! {
    static TEST_VIEWER: std::cell::Cell<Option<ViewerOverrideFn>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn resolve_viewer(instant: DateTime<Utc>) -> (ResolvedZone, ClockFormat) {
    match TEST_VIEWER.get() {
        Some(resolver) => resolver(instant),
        None => resolve_system_viewer(instant),
    }
}

/// Run `f` with a deterministic thread-local viewer timezone/clock fixture.
#[cfg(test)]
pub(crate) fn with_viewer_timezone<R>(resolver: ViewerOverrideFn, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<ViewerOverrideFn>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_VIEWER.set(self.0);
        }
    }
    let _restore = Restore(TEST_VIEWER.replace(Some(resolver)));
    f()
}

/// Deterministic test fixture: US Pacific Time (`America/Los_Angeles`) with real 2026 DST transition
/// (PDT `-07:00` between 2026-03-08 10:00Z and 2026-11-01 09:00Z, PST `-08:00` otherwise).
#[cfg(test)]
pub(crate) fn fixture_pacific_12h(instant: DateTime<Utc>) -> (ResolvedZone, ClockFormat) {
    let dst_start = DateTime::parse_from_rfc3339("2026-03-08T10:00:00Z").unwrap().with_timezone(&Utc);
    let dst_end = DateTime::parse_from_rfc3339("2026-11-01T09:00:00Z").unwrap().with_timezone(&Utc);
    let is_dst = instant >= dst_start && instant < dst_end;
    let offset_secs = if is_dst { -25_200 } else { -28_800 };
    let offset = FixedOffset::east_opt(offset_secs).unwrap();
    let label = abbreviation_for("America/Los_Angeles", offset_secs);
    (ResolvedZone { offset, label, is_utc_fallback: false }, ClockFormat::Hour12)
}

/// Deterministic test fixture: US Pacific Time with 24-hour clock.
#[cfg(test)]
pub(crate) fn fixture_pacific_24h(instant: DateTime<Utc>) -> (ResolvedZone, ClockFormat) {
    let (zone, _) = fixture_pacific_12h(instant);
    (zone, ClockFormat::Hour24)
}

/// Deterministic test fixture: Japan Standard Time (`Asia/Tokyo`, UTC+9, 24-hour clock, no DST).
#[cfg(test)]
pub(crate) fn fixture_tokyo_24h(_instant: DateTime<Utc>) -> (ResolvedZone, ClockFormat) {
    let offset = FixedOffset::east_opt(32_400).unwrap();
    let label = abbreviation_for("Asia/Tokyo", 32_400);
    (ResolvedZone { offset, label, is_utc_fallback: false }, ClockFormat::Hour24)
}

/// Deterministic test fixture: Central European Time (`Europe/Berlin`, CET/CEST, 24-hour clock).
#[cfg(test)]
pub(crate) fn fixture_berlin_24h(instant: DateTime<Utc>) -> (ResolvedZone, ClockFormat) {
    let dst_start = DateTime::parse_from_rfc3339("2026-03-29T01:00:00Z").unwrap().with_timezone(&Utc);
    let dst_end = DateTime::parse_from_rfc3339("2026-10-25T01:00:00Z").unwrap().with_timezone(&Utc);
    let offset_secs = if instant >= dst_start && instant < dst_end { 7_200 } else { 3_600 };
    let offset = FixedOffset::east_opt(offset_secs).unwrap();
    let label = abbreviation_for("Europe/Berlin", offset_secs);
    (ResolvedZone { offset, label, is_utc_fallback: false }, ClockFormat::Hour24)
}

/// Deterministic test fixture: explicit UTC fallback when system timezone data is unavailable.
#[cfg(test)]
pub(crate) fn fixture_unavailable_utc(_instant: DateTime<Utc>) -> (ResolvedZone, ClockFormat) {
    let offset = FixedOffset::east_opt(0).unwrap();
    (ResolvedZone { offset, label: Some("UTC"), is_utc_fallback: true }, ClockFormat::Hour12)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = "2026-11-19T04:43:34.946Z";
    const SUMMER_EXAMPLE: &str = "2026-07-15T18:30:12.500Z";

    #[test]
    fn supplied_gallery_example_formats_with_date_rollover_and_dst_awareness() {
        with_viewer_timezone(fixture_pacific_12h, || {
            let winter = format_compact(EXAMPLE);
            assert_eq!(winter.display, "Nov 18, 2026, 8:43 PM PST");
            assert!(winter.tooltip.as_deref().unwrap().contains(EXAMPLE));
            assert!(winter.tooltip.as_deref().unwrap().contains("8:43:34 PM PST"));

            let summer = format_compact(SUMMER_EXAMPLE);
            assert_eq!(summer.display, "Jul 15, 2026, 11:30 AM PDT");
            assert!(summer.tooltip.as_deref().unwrap().contains(SUMMER_EXAMPLE));

            // Noon and midnight edge cases in 12-hour format.
            assert_eq!(format_compact("2026-11-19T08:05:00Z").display, "Nov 19, 2026, 12:05 AM PST");
            assert_eq!(format_compact("2026-11-19T20:05:00Z").display, "Nov 19, 2026, 12:05 PM PST");
        });
    }

    #[test]
    fn clock_format_supports_both_12_hour_and_24_hour_conventions() {
        with_viewer_timezone(fixture_pacific_12h, || {
            assert_eq!(format_compact(EXAMPLE).display, "Nov 18, 2026, 8:43 PM PST");
        });
        with_viewer_timezone(fixture_pacific_24h, || {
            assert_eq!(format_compact(EXAMPLE).display, "Nov 18, 2026, 20:43 PST");
            assert_eq!(format_compact(SUMMER_EXAMPLE).display, "Jul 15, 2026, 11:30 PDT");
        });
        assert_eq!(clock_from_windows_pattern("h:mm tt"), Some(ClockFormat::Hour12));
        assert_eq!(clock_from_windows_pattern("HH:mm"), Some(ClockFormat::Hour24));
        assert_eq!(clock_from_windows_pattern("H:mm"), Some(ClockFormat::Hour24));
        assert_eq!(clock_from_windows_pattern("HH'h'mm"), Some(ClockFormat::Hour24));
        assert_eq!(clock_from_windows_pattern("'H' h:mm tt"), Some(ClockFormat::Hour12));
        assert_eq!(clock_from_locale_tag("en-US"), ClockFormat::Hour12);
        assert_eq!(clock_from_locale_tag("de-DE"), ClockFormat::Hour24);
        assert_eq!(clock_from_locale_tag("ja-JP"), ClockFormat::Hour24);
    }

    #[test]
    fn displayed_time_follows_viewer_timezone_rather_than_creator_offset() {
        // Same physical instant encoded in UTC vs Eastern (-05:00) vs Tokyo (+09:00)
        // must always display in the viewing PC's timezone.
        let same_instant_variants = ["2026-11-19T04:43:34.946Z", "2026-11-18T23:43:34.946-05:00", "2026-11-19T13:43:34.946+09:00"];
        with_viewer_timezone(fixture_pacific_12h, || {
            for raw in same_instant_variants {
                assert_eq!(format_compact(raw).display, "Nov 18, 2026, 8:43 PM PST", "{raw}");
            }
        });
        with_viewer_timezone(fixture_tokyo_24h, || {
            for raw in same_instant_variants {
                assert_eq!(format_compact(raw).display, "Nov 19, 2026, 13:43 JST", "{raw}");
            }
        });
        with_viewer_timezone(fixture_berlin_24h, || {
            assert_eq!(format_compact(EXAMPLE).display, "Nov 19, 2026, 05:43 CET");
            assert_eq!(format_compact(SUMMER_EXAMPLE).display, "Jul 15, 2026, 20:30 CEST");
        });
    }

    #[test]
    fn unavailable_timezone_fallback_explicitly_labels_utc_never_local() {
        with_viewer_timezone(fixture_unavailable_utc, || {
            let formatted = format_compact(EXAMPLE);
            assert_eq!(formatted.display, "Nov 19, 2026, 4:43 AM UTC");
            assert!(formatted.display.ends_with(" UTC"));
            let tooltip = formatted.tooltip.unwrap();
            assert!(tooltip.contains("system timezone unavailable"));
            assert!(tooltip.contains(EXAMPLE));
        });
    }

    #[test]
    fn invalid_missing_oversized_and_date_only_values_never_crash() {
        with_viewer_timezone(fixture_pacific_12h, || {
            for bad in ["", "   ", "not-a-date", "2026-99-99T00:00:00Z", "2026-11-19T25:00:00Z", &"2".repeat(256)] {
                let res = format_compact(bad);
                assert_eq!(res.display, UNAVAILABLE_LABEL, "{bad:?}");
                assert!(res.tooltip.is_none());
            }
            let date_only = format_compact("2026-11-06");
            assert_eq!(date_only.display, "Nov 6, 2026");
            assert_eq!(date_only.tooltip.as_deref(), Some("Date: 2026-11-06"));

            assert_eq!(format_unix_ms(0).display, UNAVAILABLE_LABEL);
            assert_eq!(format_unix_ms(u64::MAX).display, UNAVAILABLE_LABEL);
            let ms = DateTime::parse_from_rfc3339(EXAMPLE).unwrap().timestamp_millis() as u64;
            assert_eq!(format_unix_ms(ms).display, "Nov 18, 2026, 8:43 PM PST");
        });
    }

    #[test]
    fn mismatched_zone_key_and_offset_falls_back_to_exact_numeric_utc_offset() {
        assert_eq!(abbreviation_for("Pacific Standard Time", -28_800), Some("PST"));
        assert_eq!(abbreviation_for("Pacific Standard Time", -25_200), Some("PDT"));
        assert_eq!(abbreviation_for("Pacific Standard Time", -18_000), None);
        assert_eq!(format_utc_offset(0), "UTC");
        assert_eq!(format_utc_offset(-28_800), "UTC-8");
        assert_eq!(format_utc_offset(19_800), "UTC+5:30");
        assert_eq!(format_utc_offset(-12_600), "UTC-3:30");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn real_system_timezone_formats_without_panicking_and_preserves_input() {
        let raw = EXAMPLE.to_owned();
        let formatted = format_compact(&raw);
        assert_eq!(raw, EXAMPLE, "formatting never mutates stored timestamp");
        assert_ne!(formatted.display, UNAVAILABLE_LABEL);
        assert!(!formatted.display.contains("2026-11-19T"), "compact label replaces raw ISO T syntax: {}", formatted.display);
        assert!(!formatted.display.contains(".946"), "compact label omits fractional seconds: {}", formatted.display);
        assert!(formatted.tooltip.as_deref().unwrap().contains(EXAMPLE));
    }
}
