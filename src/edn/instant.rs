//! RFC 3339 validation without rounding the retained fractional seconds.

use crate::SemanticError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstantParts {
    /// UTC second, or the preceding `:59` for a leap second.
    pub seconds: i64,
    pub fraction: String,
    pub leap_second: bool,
}

fn invalid() -> SemanticError {
    SemanticError::incorrect("edn/instant", "expected a valid RFC 3339 timestamp")
}

fn number(bytes: &[u8]) -> Result<i64, SemanticError> {
    bytes.iter().try_fold(0, |value, byte| {
        if byte.is_ascii_digit() {
            Ok(value * 10 + i64::from(byte - b'0'))
        } else {
            Err(invalid())
        }
    })
}

fn leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn month_days(year: i64, month: i64) -> i64 {
    match month {
        2 => {
            if leap(year) {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    }
}

// Proleptic Gregorian civil-date algorithms. Euclidean division includes year0.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    era * 146097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year - 719468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719468;
    let era = days.div_euclid(146097);
    let day_of_era = days - era * 146097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = shifted_month + if shifted_month < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month, day)
}

pub(crate) fn parse_instant(text: &str) -> Result<InstantParts, SemanticError> {
    let bytes = text.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(invalid());
    }
    let year = number(&bytes[..4])?;
    let month = number(&bytes[5..7])?;
    let day = number(&bytes[8..10])?;
    let hour = number(&bytes[11..13])?;
    let minute = number(&bytes[14..16])?;
    let second = number(&bytes[17..19])?;
    if day < 1 || day > month_days(year, month) || hour > 23 || minute > 59 || second > 60 {
        return Err(invalid());
    }
    let mut position = 19;
    let fraction = if bytes.get(position) == Some(&b'.') {
        position += 1;
        let start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if start == position {
            return Err(invalid());
        }
        text[start..position].to_owned()
    } else {
        String::new()
    };
    let offset = match bytes.get(position) {
        Some(b'Z' | b'z') if position + 1 == bytes.len() => 0,
        Some(sign @ (b'+' | b'-'))
            if position + 6 == bytes.len() && bytes[position + 3] == b':' =>
        {
            let hours = number(&bytes[position + 1..position + 3])?;
            let minutes = number(&bytes[position + 4..position + 6])?;
            if hours > 23 || minutes > 59 {
                return Err(invalid());
            }
            (hours * 3600 + minutes * 60) * if *sign == b'+' { 1 } else { -1 }
        }
        _ => return Err(invalid()),
    };
    let seconds =
        days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second.min(59)
            - offset;
    let leap_second = second == 60;
    if leap_second {
        // RFC3339 permits leap seconds only at the end of a UTC month. The
        // historical/future leap schedule is not inferred from a syntax reader.
        let (utc_year, utc_month, utc_day) = civil_from_days(seconds.div_euclid(86400));
        if seconds.rem_euclid(86400) != 86399 || utc_day != month_days(utc_year, utc_month) {
            return Err(invalid());
        }
    }
    Ok(InstantParts {
        seconds,
        fraction,
        leap_second,
    })
}

pub(crate) fn format_millis(millis: i64) -> Option<String> {
    let seconds = millis.div_euclid(1000);
    let (year, month, day) = civil_from_days(seconds.div_euclid(86400));
    if !(0..=9999).contains(&year) {
        return None;
    }
    let within_day = seconds.rem_euclid(86400);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        within_day / 3600,
        within_day / 60 % 60,
        within_day % 60,
        millis.rem_euclid(1000)
    ))
}
