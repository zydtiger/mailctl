use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};

/// Parses a `--since`/`--before` argument into Unix seconds.
pub fn parse_time(input: &str, now: DateTime<Local>) -> Result<i64> {
    let text = input.trim();
    match text {
        "now" => return Ok(now.timestamp()),
        "today" => return local_midnight(now.date_naive()),
        "yesterday" => return local_midnight(now.date_naive() - Duration::days(1)),
        _ => {}
    }
    if let Some(ago) = parse_relative(text) {
        return Ok((now - ago).timestamp());
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return local_midnight(date);
    }
    DateTime::parse_from_rfc3339(text)
        .map(|value| value.timestamp())
        .with_context(|| {
            format!(
                "invalid time `{input}`: use YYYY-MM-DD, RFC 3339, today, yesterday, or a duration such as 3h, 2d, 1w"
            )
        })
}

fn parse_relative(text: &str) -> Option<Duration> {
    let unit = text.chars().last()?;
    let count: i64 = text.strip_suffix(unit)?.parse().ok()?;
    match unit {
        'h' => Some(Duration::hours(count)),
        'd' => Some(Duration::days(count)),
        'w' => Some(Duration::weeks(count)),
        _ => None,
    }
}

fn local_midnight(date: NaiveDate) -> Result<i64> {
    let naive = date.and_hms_opt(0, 0, 0).context("invalid date")?;
    match Local.from_local_datetime(&naive).earliest() {
        Some(value) => Ok(value.timestamp()),
        None => bail!("{date} has no local midnight"),
    }
}

/// Formats Unix seconds as local RFC 3339 for JSON output.
pub fn rfc3339(seconds: i64) -> Option<String> {
    to_local(seconds).map(|value| value.to_rfc3339())
}

/// Formats Unix seconds as a short local timestamp for tables.
pub fn short(seconds: i64) -> String {
    to_local(seconds)
        .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn to_local(seconds: i64) -> Option<DateTime<Local>> {
    DateTime::<Utc>::from_timestamp(seconds, 0).map(|value| value.with_timezone(&Local))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 2, 15, 30, 0).unwrap()
    }

    #[test]
    fn parses_relative_and_named_times() {
        let now = now();
        assert_eq!(
            parse_time("2d", now).unwrap(),
            (now - Duration::days(2)).timestamp()
        );
        assert_eq!(
            parse_time("3h", now).unwrap(),
            (now - Duration::hours(3)).timestamp()
        );
        assert_eq!(
            parse_time("today", now).unwrap(),
            Local
                .with_ymd_and_hms(2026, 11, 2, 0, 0, 0)
                .unwrap()
                .timestamp()
        );
        assert_eq!(
            parse_time("yesterday", now).unwrap(),
            Local
                .with_ymd_and_hms(2026, 11, 1, 0, 0, 0)
                .unwrap()
                .timestamp()
        );
    }

    #[test]
    fn parses_dates_and_rfc3339() {
        let now = now();
        assert_eq!(
            parse_time("2026-10-01", now).unwrap(),
            Local
                .with_ymd_and_hms(2026, 10, 1, 0, 0, 0)
                .unwrap()
                .timestamp()
        );
        assert_eq!(
            parse_time("2026-11-02T00:00:00Z", now).unwrap(),
            1_793_577_600
        );
        assert!(parse_time("soon", now).is_err());
        assert!(parse_time("d", now).is_err());
        assert!(parse_time("昨天", now).is_err());
    }
}
