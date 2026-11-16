//! `Date`, in UTC: a script reads the same clock whatever the host's time
//! zone, as pi's deterministic runs do. A date is its time in milliseconds
//! since the epoch; an invalid one is `NaN`.

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, Timelike, Utc};

use crate::interp::{Interp, R};
use crate::value::{Value, date_iso, to_number};

pub const METHODS: &[&str] = &[
    "getTime",
    "valueOf",
    "toISOString",
    "toJSON",
    "getFullYear",
    "getMonth",
    "getDate",
    "getDay",
    "getHours",
    "getMinutes",
    "getSeconds",
    "getMilliseconds",
    "getUTCFullYear",
    "getUTCMonth",
    "getUTCDate",
    "getUTCDay",
    "getUTCHours",
    "getUTCMinutes",
    "getUTCSeconds",
    "getUTCMilliseconds",
    "getTimezoneOffset",
    "toString",
    "toDateString",
    "toTimeString",
    "toUTCString",
    "toLocaleDateString",
    "toLocaleTimeString",
    "toLocaleString",
];

pub fn now() -> f64 {
    Utc::now().timestamp_millis() as f64
}

fn datetime(time: f64) -> Option<DateTime<Utc>> {
    if !time.is_finite() {
        return None;
    }
    DateTime::<Utc>::from_timestamp_millis(time as i64)
}

/// `Date.parse`: ISO dates and times (a time without a zone is UTC here),
/// and RFC 2822.
pub fn parse(text: &str) -> f64 {
    let text = text.trim();
    if let Ok(date) = DateTime::parse_from_rfc3339(text) {
        return date.timestamp_millis() as f64;
    }
    if let Ok(date) = DateTime::parse_from_rfc2822(text) {
        return date.timestamp_millis() as f64;
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(date) = NaiveDateTime::parse_from_str(text, format) {
            return date.and_utc().timestamp_millis() as f64;
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return date
            .and_hms_opt(0, 0, 0)
            .map_or(f64::NAN, |date| date.and_utc().timestamp_millis() as f64);
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m") {
        return date
            .and_hms_opt(0, 0, 0)
            .map_or(f64::NAN, |date| date.and_utc().timestamp_millis() as f64);
    }
    f64::NAN
}

/// `Date.UTC(year, month, day, hours, minutes, seconds, ms)`, month from 0;
/// fields past their range carry over, as JavaScript's do.
pub fn from_parts(args: &[Value]) -> f64 {
    let field = |index: usize, default: f64| match args.get(index) {
        Some(value) => to_number(value),
        None => default,
    };
    let fields = [
        field(0, f64::NAN),
        field(1, 0.0),
        field(2, 1.0),
        field(3, 0.0),
        field(4, 0.0),
        field(5, 0.0),
        field(6, 0.0),
    ];
    if fields.iter().any(|value| !value.is_finite()) {
        return f64::NAN;
    }
    let mut year = fields[0].trunc();
    if (0.0..=99.0).contains(&year) {
        year += 1900.0;
    }
    let month = fields[1].trunc();
    let year = year + (month / 12.0).floor();
    let month = month.rem_euclid(12.0);
    let Some(first) = NaiveDate::from_ymd_opt(year as i32, month as u32 + 1, 1) else {
        return f64::NAN;
    };
    let Some(midnight) = first.and_hms_opt(0, 0, 0) else {
        return f64::NAN;
    };
    let base = midnight.and_utc().timestamp_millis() as f64;
    base + (fields[2].trunc() - 1.0) * 86_400_000.0
        + fields[3].trunc() * 3_600_000.0
        + fields[4].trunc() * 60_000.0
        + fields[5].trunc() * 1000.0
        + fields[6].trunc()
}

/// `new Date(...)`.
pub fn construct(args: &[Value]) -> f64 {
    match args {
        [] => now(),
        [Value::Date(time)] => *time,
        [Value::Str(text)] => parse(text),
        [value] => {
            let time = to_number(value);
            if time.is_finite() && time.abs() <= 8.64e15 {
                time.trunc()
            } else {
                f64::NAN
            }
        }
        args => from_parts(args),
    }
}

/// What `Date()` and `toString()` print.
pub fn date_string(time: f64) -> String {
    datetime(time).map_or_else(
        || "Invalid Date".to_owned(),
        |date| {
            date.format("%a %b %d %Y %H:%M:%S GMT+0000 (Coordinated Universal Time)")
                .to_string()
        },
    )
}

fn hour12(date: &DateTime<Utc>) -> String {
    let (pm, hour) = date.hour12();
    format!(
        "{hour}:{:02}:{:02} {}",
        date.minute(),
        date.second(),
        if pm { "PM" } else { "AM" }
    )
}

pub fn date_method(
    interp: &mut Interp<'_>,
    method: &str,
    this: &Value,
    args: &[Value],
) -> R<Value> {
    let Value::Date(time) = this else {
        return Err(interp.type_error(format!(
            "Date.prototype.{method} called on a value that is not a Date"
        )));
    };
    let _ = args;
    let time = *time;
    let Some(date) = datetime(time) else {
        return match method {
            "getTime" | "valueOf" | "getTimezoneOffset" => Ok(Value::Number(f64::NAN)),
            "toISOString" => Err(interp.throw("RangeError", "Invalid time value")),
            "toJSON" => Ok(Value::Null),
            name if name.starts_with("get") => Ok(Value::Number(f64::NAN)),
            _ => Ok(Value::str("Invalid Date")),
        };
    };
    let number = |value: u32| Ok(Value::Number(f64::from(value)));
    match method {
        "getTime" | "valueOf" => Ok(Value::Number(time)),
        "toISOString" | "toJSON" => Ok(Value::str(date_iso(time).unwrap_or_default())),
        "getFullYear" | "getUTCFullYear" => Ok(Value::Number(f64::from(date.year()))),
        "getMonth" | "getUTCMonth" => number(date.month0()),
        "getDate" | "getUTCDate" => number(date.day()),
        "getDay" | "getUTCDay" => number(date.weekday().num_days_from_sunday()),
        "getHours" | "getUTCHours" => number(date.hour()),
        "getMinutes" | "getUTCMinutes" => number(date.minute()),
        "getSeconds" | "getUTCSeconds" => number(date.second()),
        "getMilliseconds" | "getUTCMilliseconds" => number(date.timestamp_subsec_millis()),
        "getTimezoneOffset" => Ok(Value::Number(0.0)),
        "toDateString" => Ok(Value::str(date.format("%a %b %d %Y").to_string())),
        "toTimeString" => Ok(Value::str(
            date.format("%H:%M:%S GMT+0000 (Coordinated Universal Time)")
                .to_string(),
        )),
        "toUTCString" => Ok(Value::str(
            date.format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
        )),
        "toLocaleDateString" => Ok(Value::str(format!(
            "{}/{}/{}",
            date.month(),
            date.day(),
            date.year()
        ))),
        "toLocaleTimeString" => Ok(Value::str(hour12(&date))),
        "toLocaleString" => Ok(Value::str(format!(
            "{}/{}/{}, {}",
            date.month(),
            date.day(),
            date.year(),
            hour12(&date)
        ))),
        _ => Ok(Value::str(date_string(time))),
    }
}

/// A `Utc` time for tests.
#[cfg(test)]
pub fn at(year: i32, month: u32, day: u32) -> f64 {
    use chrono::TimeZone as _;
    Utc.with_ymd_and_hms(year, month, day, 0, 0, 0)
        .single()
        .map_or(f64::NAN, |date| date.timestamp_millis() as f64)
}

#[cfg(test)]
mod tests {
    use super::{at, from_parts, parse};
    use crate::value::Value;

    #[test]
    fn parses_iso_and_builds_from_parts() {
        assert_eq!(parse("2026-11-16"), at(2026, 11, 16));
        assert_eq!(parse("2026-11-16T00:00:00Z"), at(2026, 11, 16));
        assert_eq!(parse("2026-11-16T07:00:00+07:00"), at(2026, 11, 16));
        assert!(parse("not a date").is_nan());
        assert_eq!(
            from_parts(&[
                Value::Number(2026.0),
                Value::Number(10.0),
                Value::Number(16.0)
            ]),
            at(2026, 11, 16)
        );
        // Month 12 is January of the next year.
        assert_eq!(
            from_parts(&[
                Value::Number(2026.0),
                Value::Number(12.0),
                Value::Number(1.0)
            ]),
            at(2027, 1, 1)
        );
    }
}
