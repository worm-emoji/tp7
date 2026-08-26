//! DateTime struct and serialization for MTP/PTP.

use super::{pack_string, unpack_string};

/// Date and time structure for MTP/PTP.
///
/// Format: "YYYYMMDDThhmmss" (ISO 8601 subset)
///
/// # Validation
///
/// DateTime values must satisfy these constraints:
/// - Year: 0-9999 (4-digit representation)
/// - Month: 1-12
/// - Day: 1-31
/// - Hour: 0-23
/// - Minute: 0-59
/// - Second: 0-59
///
/// Use [`DateTime::new()`] to create validated instances, or [`DateTime::is_valid()`]
/// to check existing instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DateTime {
    /// Year (0-9999)
    pub year: u16,
    /// Month (1-12)
    pub month: u8,
    /// Day (1-31)
    pub day: u8,
    /// Hour (0-23)
    pub hour: u8,
    /// Minute (0-59)
    pub minute: u8,
    /// Second (0-59)
    pub second: u8,
}

impl DateTime {
    /// Create a new DateTime with validation.
    ///
    /// Returns `None` if any value is out of range.
    ///
    /// # Example
    ///
    /// ```
    /// use mtp_rs::ptp::DateTime;
    ///
    /// let dt = DateTime::new(2024, 3, 15, 14, 30, 22).unwrap();
    /// assert_eq!(dt.year, 2024);
    ///
    /// // Invalid values return None
    /// assert!(DateTime::new(2024, 13, 1, 0, 0, 0).is_none()); // month > 12
    /// assert!(DateTime::new(2024, 1, 1, 0, 60, 0).is_none()); // minute > 59
    /// ```
    #[must_use]
    pub fn new(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Option<Self> {
        let dt = DateTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        };
        if dt.is_valid() {
            Some(dt)
        } else {
            None
        }
    }

    /// Check if this DateTime has valid values.
    ///
    /// Returns `true` if all fields are within valid ranges:
    /// - Year: 0-9999
    /// - Month: 1-12
    /// - Day: 1-31
    /// - Hour: 0-23
    /// - Minute: 0-59
    /// - Second: 0-59
    ///
    /// Note: This does not validate day-of-month against the specific month
    /// (e.g., Feb 31 would pass). MTP devices generally accept any 1-31 value.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.year <= 9999
            && (1..=12).contains(&self.month)
            && (1..=31).contains(&self.day)
            && self.hour <= 23
            && self.minute <= 59
            && self.second <= 59
    }

    /// Parse a datetime string in MTP format.
    ///
    /// Format: "YYYYMMDDThhmmss" with optional timezone suffix (Z or +hhmm/-hhmm).
    /// The timezone suffix is parsed but ignored.
    ///
    /// Returns `None` if the string is malformed or contains invalid values.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        // Minimum length: "YYYYMMDDThhmmss" = 15 characters
        if s.len() < 15 {
            return None;
        }

        // Check for 'T' separator at position 8
        if s.as_bytes().get(8) != Some(&b'T') {
            return None;
        }

        // Parse components
        let year: u16 = s.get(0..4)?.parse().ok()?;
        let month: u8 = s.get(4..6)?.parse().ok()?;
        let day: u8 = s.get(6..8)?.parse().ok()?;
        let hour: u8 = s.get(9..11)?.parse().ok()?;
        let minute: u8 = s.get(11..13)?.parse().ok()?;
        let second: u8 = s.get(13..15)?.parse().ok()?;

        // Use new() which validates
        Self::new(year, month, day, hour, minute, second)
    }

    /// Format the datetime as an MTP string.
    ///
    /// Returns `Some("YYYYMMDDThhmmss")` if the values are valid (exactly 15 characters),
    /// or `None` if any value is out of range.
    ///
    /// # Example
    ///
    /// ```
    /// use mtp_rs::ptp::DateTime;
    ///
    /// let dt = DateTime::new(2024, 3, 15, 14, 30, 22).unwrap();
    /// assert_eq!(dt.format(), Some("20240315T143022".to_string()));
    ///
    /// // Invalid DateTime returns None
    /// let invalid = DateTime { year: 2024, month: 13, day: 1, hour: 0, minute: 0, second: 0 };
    /// assert_eq!(invalid.format(), None);
    /// ```
    #[must_use]
    pub fn format(&self) -> Option<String> {
        if !self.is_valid() {
            return None;
        }
        Some(format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        ))
    }
}

/// Pack a DateTime into MTP string format.
///
/// Returns an error if the DateTime contains invalid values.
pub fn pack_datetime(dt: &DateTime) -> Result<Vec<u8>, crate::Error> {
    let formatted = dt.format().ok_or_else(|| {
        crate::Error::invalid_data(format!(
            "invalid DateTime: year={}, month={}, day={}, hour={}, minute={}, second={}",
            dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
        ))
    })?;
    Ok(pack_string(&formatted))
}

/// Unpack a DateTime from a buffer.
///
/// Returns the datetime (or None for empty string) and the number of bytes consumed.
pub fn unpack_datetime(buf: &[u8]) -> Result<(Option<DateTime>, usize), crate::Error> {
    let (s, consumed) = unpack_string(buf)?;

    if s.is_empty() {
        return Ok((None, consumed));
    }

    let dt = DateTime::parse(&s)
        .ok_or_else(|| crate::Error::invalid_data(format!("invalid datetime format: {}", s)))?;

    Ok((Some(dt), consumed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // --- DateTime parsing tests ---

    #[test]
    fn datetime_parse_basic() {
        let dt = DateTime::parse("20240315T143022").unwrap();
        assert_eq!(
            (dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second),
            (2024, 3, 15, 14, 30, 22)
        );
    }

    #[test]
    fn datetime_parse_with_timezone_z() {
        assert!(DateTime::parse("20240315T143022Z").is_some());
    }

    #[test]
    fn datetime_parse_with_timezone_positive() {
        assert!(DateTime::parse("20240315T143022+0530").is_some());
    }

    #[test]
    fn datetime_parse_with_timezone_negative() {
        assert!(DateTime::parse("20240315T143022-0800").is_some());
    }

    #[test]
    fn datetime_parse_invalid_too_short() {
        for s in ["2024031", ""] {
            assert!(DateTime::parse(s).is_none());
        }
    }

    #[test]
    fn datetime_parse_invalid_no_t_separator() {
        for s in ["20240315 143022", "20240315143022"] {
            assert!(DateTime::parse(s).is_none());
        }
    }

    #[test]
    fn datetime_parse_invalid_month() {
        for s in ["20240015T143022", "20241315T143022"] {
            // month=0, month=13
            assert!(DateTime::parse(s).is_none());
        }
    }

    #[test]
    fn datetime_parse_invalid_day() {
        for s in ["20240100T143022", "20240132T143022"] {
            // day=0, day=32
            assert!(DateTime::parse(s).is_none());
        }
    }

    #[test]
    fn datetime_parse_invalid_hour() {
        assert!(DateTime::parse("20240315T243022").is_none()); // hour=24
    }

    #[test]
    fn datetime_parse_invalid_minute() {
        assert!(DateTime::parse("20240315T146022").is_none()); // minute=60
    }

    #[test]
    fn datetime_parse_invalid_second() {
        assert!(DateTime::parse("20240315T143060").is_none()); // second=60
    }

    // --- DateTime format tests ---

    #[test]
    fn datetime_format() {
        assert_eq!(
            DateTime::new(2024, 3, 15, 14, 30, 22).unwrap().format(),
            Some("20240315T143022".into())
        );
    }

    #[test]
    fn datetime_format_with_leading_zeros() {
        assert_eq!(
            DateTime::new(2024, 1, 5, 9, 5, 3).unwrap().format(),
            Some("20240105T090503".into())
        );
    }

    #[test]
    fn datetime_roundtrip() {
        let original = DateTime::new(2024, 12, 31, 23, 59, 59).unwrap();
        assert_eq!(
            DateTime::parse(&original.format().unwrap()).unwrap(),
            original
        );
    }

    #[test]
    fn datetime_format_invalid_returns_none() {
        let invalid_cases = [
            (2024, 13, 1, 0, 0, 0), // invalid month
            (2024, 1, 1, 0, 60, 0), // invalid minute
            (10000, 1, 1, 0, 0, 0), // year too large
        ];
        for (y, mo, d, h, mi, s) in invalid_cases {
            let dt = DateTime {
                year: y,
                month: mo,
                day: d,
                hour: h,
                minute: mi,
                second: s,
            };
            assert_eq!(dt.format(), None);
        }
    }

    #[test]
    fn datetime_default() {
        let dt = DateTime::default();
        assert_eq!(
            (dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second),
            (0, 0, 0, 0, 0, 0)
        );
    }

    // --- Pack/unpack tests ---

    #[test]
    fn pack_datetime_basic() {
        let packed = pack_datetime(&DateTime::new(2024, 3, 15, 14, 30, 22).unwrap()).unwrap();
        assert_eq!(packed[0], 16); // 15 chars + null terminator
    }

    #[test]
    fn pack_datetime_invalid_returns_error() {
        let invalid = DateTime {
            year: 2024,
            month: 13,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert!(pack_datetime(&invalid).is_err());
    }

    #[test]
    fn unpack_datetime_basic() {
        let dt = DateTime::new(2024, 3, 15, 14, 30, 22).unwrap();
        let (unpacked, _) = unpack_datetime(&pack_datetime(&dt).unwrap()).unwrap();
        assert_eq!(unpacked, Some(dt));
    }

    #[test]
    fn unpack_datetime_empty_string() {
        let (dt, consumed) = unpack_datetime(&[0x00]).unwrap();
        assert_eq!((dt, consumed), (None, 1));
    }

    #[test]
    fn unpack_datetime_invalid_format() {
        assert!(unpack_datetime(&pack_string("not a date")).is_err());
    }

    #[test]
    fn datetime_pack_unpack_roundtrip() {
        for dt in [
            DateTime::new(2024, 1, 1, 0, 0, 0).unwrap(),
            DateTime::new(2024, 12, 31, 23, 59, 59).unwrap(),
            DateTime::new(1999, 6, 15, 12, 30, 45).unwrap(),
        ] {
            let (unpacked, _) = unpack_datetime(&pack_datetime(&dt).unwrap()).unwrap();
            assert_eq!(unpacked, Some(dt));
        }
    }

    // --- Boundary tests ---

    #[test]
    fn datetime_boundary_day_31() {
        let dt = DateTime {
            year: 2024,
            month: 1,
            day: 31,
            hour: 0,
            minute: 0,
            second: 0,
        };
        let parsed = DateTime::parse(&dt.format().unwrap()).unwrap();
        assert_eq!(parsed.day, 31);
    }

    #[test]
    fn datetime_boundary_year_0() {
        let dt = DateTime {
            year: 0,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
        };
        if let Some(p) = DateTime::parse(&dt.format().unwrap()) {
            assert_eq!(p.year, 0);
        }
    }

    #[test]
    fn datetime_boundary_year_9999() {
        let dt = DateTime {
            year: 9999,
            month: 12,
            day: 31,
            hour: 23,
            minute: 59,
            second: 59,
        };
        assert_eq!(DateTime::parse(&dt.format().unwrap()).unwrap().year, 9999);
    }

    #[test]
    fn datetime_boundary_year_10000() {
        let dt = DateTime {
            year: 10000,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert!(dt.format().is_none());
        assert!(pack_datetime(&dt).is_err());
    }

    // --- Property-based tests ---

    fn valid_datetime() -> impl Strategy<Value = DateTime> {
        (
            1000u16..9999u16,
            1u8..=12u8,
            1u8..=28u8,
            0u8..=23u8,
            0u8..=59u8,
            0u8..=59u8,
        )
            .prop_map(|(year, month, day, hour, minute, second)| DateTime {
                year,
                month,
                day,
                hour,
                minute,
                second,
            })
    }

    proptest! {
        #[test]
        fn prop_datetime_format_parse_roundtrip(dt in valid_datetime()) {
            let formatted = dt.format().unwrap();
            prop_assert_eq!(DateTime::parse(&formatted).unwrap(), dt);
        }

        #[test]
        fn prop_datetime_pack_unpack_roundtrip(dt in valid_datetime()) {
            let packed = pack_datetime(&dt).unwrap();
            let (unpacked, consumed) = unpack_datetime(&packed).unwrap();
            prop_assert_eq!(unpacked, Some(dt));
            prop_assert_eq!(consumed, packed.len());
        }

        #[test]
        fn prop_datetime_format_length(dt in valid_datetime()) {
            prop_assert_eq!(dt.format().unwrap().len(), 15);
        }

        #[test]
        fn fuzz_datetime_invalid_month(
            year in 1900u16..2100u16,
            month in prop::sample::select(vec![0u8, 13, 14, 99, 255]),
            day in 1u8..=28u8, hour in 0u8..=23u8, minute in 0u8..=59u8, second in 0u8..=59u8,
        ) {
            let dt = DateTime { year, month, day, hour, minute, second };
            prop_assert!(dt.format().is_none());
            prop_assert!(pack_datetime(&dt).is_err());
        }

        #[test]
        fn fuzz_datetime_invalid_day(
            year in 1900u16..2100u16, month in 1u8..=12u8,
            day in prop::sample::select(vec![0u8, 32, 33, 99, 255]),
            hour in 0u8..=23u8, minute in 0u8..=59u8, second in 0u8..=59u8,
        ) {
            let dt = DateTime { year, month, day, hour, minute, second };
            prop_assert!(dt.format().is_none());
            prop_assert!(pack_datetime(&dt).is_err());
        }

        #[test]
        fn fuzz_datetime_invalid_hour(
            year in 1900u16..2100u16, month in 1u8..=12u8, day in 1u8..=28u8,
            hour in prop::sample::select(vec![24u8, 25, 99, 255]),
            minute in 0u8..=59u8, second in 0u8..=59u8,
        ) {
            let dt = DateTime { year, month, day, hour, minute, second };
            prop_assert!(dt.format().is_none());
            prop_assert!(pack_datetime(&dt).is_err());
        }

        #[test]
        fn fuzz_datetime_invalid_minute(
            year in 1900u16..2100u16, month in 1u8..=12u8, day in 1u8..=28u8, hour in 0u8..=23u8,
            minute in prop::sample::select(vec![60u8, 61, 99]),
            second in 0u8..=59u8,
        ) {
            let dt = DateTime { year, month, day, hour, minute, second };
            prop_assert!(dt.format().is_none());
            prop_assert!(pack_datetime(&dt).is_err());
        }

        #[test]
        fn fuzz_datetime_minute_overflow(
            year in 1900u16..2100u16, month in 1u8..=12u8, day in 1u8..=28u8, hour in 0u8..=23u8,
            minute in 100u8..=255u8, second in 0u8..=59u8,
        ) {
            let dt = DateTime { year, month, day, hour, minute, second };
            prop_assert!(dt.format().is_none());
            prop_assert!(pack_datetime(&dt).is_err());
        }

        #[test]
        fn fuzz_datetime_invalid_second(
            year in 1900u16..2100u16, month in 1u8..=12u8, day in 1u8..=28u8, hour in 0u8..=23u8, minute in 0u8..=59u8,
            second in prop::sample::select(vec![60u8, 61, 99]),
        ) {
            let dt = DateTime { year, month, day, hour, minute, second };
            prop_assert!(dt.format().is_none());
            prop_assert!(pack_datetime(&dt).is_err());
        }

        #[test]
        fn fuzz_datetime_second_overflow(
            year in 1900u16..2100u16, month in 1u8..=12u8, day in 1u8..=28u8, hour in 0u8..=23u8, minute in 0u8..=59u8,
            second in 100u8..=255u8,
        ) {
            let dt = DateTime { year, month, day, hour, minute, second };
            prop_assert!(dt.format().is_none());
            prop_assert!(pack_datetime(&dt).is_err());
        }

        #[test]
        fn fuzz_datetime_parse_garbage(s in ".*") {
            let _ = DateTime::parse(&s);
        }

        #[test]
        fn fuzz_datetime_parse_malformed(prefix in "[0-9]{0,20}", suffix in "[^T]*") {
            let _ = DateTime::parse(&format!("{}{}", prefix, suffix));
        }
    }

    crate::fuzz_bytes_fn!(fuzz_unpack_datetime, unpack_datetime, 50);
}
