//! Function argument values: numbers, sizes, dates, comparisons and ranges.

use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{Span, Zoned};

/// A half-open interval `[start, end)` denoted by one value (`1mb`, `2024`, `today`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interval {
    pub start: i128,
    pub end: i128,
}

impl Interval {
    #[must_use]
    pub fn point(v: i128) -> Self {
        Self {
            start: v,
            end: v + 1,
        }
    }
}

/// A constraint `lo <= x < hi` (either side may be open).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub lo: Option<i128>,
    pub hi: Option<i128>,
}

impl Range {
    #[must_use]
    pub fn contains(&self, v: i128) -> bool {
        self.lo.is_none_or(|lo| v >= lo) && self.hi.is_none_or(|hi| v < hi)
    }
}

/// Parses `[op]value` or `a..b`, where `value` is parsed by `value`.
///
/// | form | meaning |
/// |---|---|
/// | `v`, `=v` | inside `v`’s interval |
/// | `<v` | before `v` starts |
/// | `<=v` | before `v` ends |
/// | `>v` | after `v` ends |
/// | `>=v` | from `v`’s start on |
/// | `a..b` | from `a`’s start to `b`’s end (either side may be omitted) |
pub fn parse_range(
    arg: &str,
    value: &dyn Fn(&str) -> Result<Interval, String>,
) -> Result<Range, String> {
    let arg = arg.trim();
    if let Some((a, b)) = arg.split_once("..") {
        let lo = if a.is_empty() {
            None
        } else {
            Some(value(a)?.start)
        };
        let hi = if b.is_empty() {
            None
        } else {
            Some(value(b)?.end)
        };
        return Ok(Range { lo, hi });
    }
    let (op, rest) = if let Some(r) = arg.strip_prefix(">=") {
        (">=", r)
    } else if let Some(r) = arg.strip_prefix("<=") {
        ("<=", r)
    } else if let Some(r) = arg.strip_prefix('>') {
        (">", r)
    } else if let Some(r) = arg.strip_prefix('<') {
        ("<", r)
    } else if let Some(r) = arg.strip_prefix('=') {
        ("=", r)
    } else {
        ("=", arg)
    };
    let iv = value(rest.trim())?;
    Ok(match op {
        ">=" => Range {
            lo: Some(iv.start),
            hi: None,
        },
        "<=" => Range {
            lo: None,
            hi: Some(iv.end),
        },
        ">" => Range {
            lo: Some(iv.end),
            hi: None,
        },
        "<" => Range {
            lo: None,
            hi: Some(iv.start),
        },
        _ => Range {
            lo: Some(iv.start),
            hi: Some(iv.end),
        },
    })
}

/// A plain non-negative integer.
pub fn parse_number(s: &str) -> Result<Interval, String> {
    s.trim()
        .parse::<i128>()
        .map(Interval::point)
        .map_err(|_| format!("expected a number, got {s:?}"))
}

const KB: i128 = 1024;

/// Sizes: `123`, `1.5mb`, `10k`, `2GiB`, or a size constant.
///
/// Constants follow Everything: `empty` = 0; `tiny` ≤ 10 KB; `small` ≤ 100 KB; `medium` ≤ 1 MB;
/// `large` ≤ 16 MB; `huge` ≤ 128 MB; `gigantic` > 128 MB (each above the previous bucket).
pub fn parse_size(s: &str) -> Result<Interval, String> {
    let lower = s.trim().to_ascii_lowercase();
    let bucket = |lo: i128, hi: i128| Interval {
        start: lo + 1,
        end: hi + 1,
    };
    match lower.as_str() {
        "empty" => return Ok(Interval::point(0)),
        "tiny" => return Ok(bucket(0, 10 * KB)),
        "small" => return Ok(bucket(10 * KB, 100 * KB)),
        "medium" => return Ok(bucket(100 * KB, KB * KB)),
        "large" => return Ok(bucket(KB * KB, 16 * KB * KB)),
        "huge" => return Ok(bucket(16 * KB * KB, 128 * KB * KB)),
        "gigantic" => {
            return Ok(Interval {
                start: 128 * KB * KB + 1,
                end: i128::from(u64::MAX) + 1,
            });
        }
        _ => {}
    }
    let split = lower
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(lower.len());
    let (num, unit) = lower.split_at(split);
    if num.is_empty() {
        return Err(format!("expected a size, got {s:?}"));
    }
    let mult: i128 = match unit.trim() {
        "" | "b" | "byte" | "bytes" => 1,
        "k" | "kb" | "kib" => KB,
        "m" | "mb" | "mib" => KB * KB,
        "g" | "gb" | "gib" => KB * KB * KB,
        "t" | "tb" | "tib" => KB * KB * KB * KB,
        "p" | "pb" | "pib" => KB * KB * KB * KB * KB,
        other => return Err(format!("unknown size unit {other:?}")),
    };
    if let Some((int, frac)) = num.split_once('.') {
        let int: i128 = if int.is_empty() {
            0
        } else {
            int.parse().map_err(|_| format!("bad size {s:?}"))?
        };
        let digits = frac.len().min(12);
        let frac_v: i128 = if digits == 0 {
            0
        } else {
            frac[..digits]
                .parse()
                .map_err(|_| format!("bad size {s:?}"))?
        };
        let scale = 10i128.pow(u32::try_from(digits).unwrap_or(0));
        let v = int * mult + frac_v * mult / scale;
        return Ok(Interval::point(v));
    }
    let v: i128 = num.parse().map_err(|_| format!("bad size {s:?}"))?;
    Ok(Interval::point(v * mult))
}

/// The clock and time zone date constants are evaluated against.
#[derive(Clone, Debug)]
pub struct Clock {
    pub now: Zoned,
}

impl Clock {
    /// The current time in the system time zone.
    #[must_use]
    pub fn system() -> Self {
        Self { now: Zoned::now() }
    }

    /// A fixed time in a given zone (tests, or a client’s zone in the daemon).
    #[must_use]
    pub fn at(unix_secs: i64, tz: TimeZone) -> Self {
        let ts = jiff::Timestamp::from_second(unix_secs).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
        Self {
            now: ts.to_zoned(tz),
        }
    }

    fn tz(&self) -> TimeZone {
        self.now.time_zone().clone()
    }

    fn today(&self) -> Date {
        self.now.date()
    }

    fn midnight(&self, d: Date) -> Result<i128, String> {
        d.to_zoned(self.tz())
            .map(|z| i128::from(z.timestamp().as_second()))
            .map_err(|e| e.to_string())
    }

    fn day_range(&self, from: Date, to_exclusive: Date) -> Result<Interval, String> {
        Ok(Interval {
            start: self.midnight(from)?,
            end: self.midnight(to_exclusive)?,
        })
    }

    fn now_secs(&self) -> i128 {
        i128::from(self.now.timestamp().as_second())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Year,
    Quarter,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
}

fn parse_unit(s: &str) -> Option<Unit> {
    Some(match s {
        "y" | "year" | "years" => Unit::Year,
        "quarter" | "quarters" => Unit::Quarter,
        "mon" | "month" | "months" => Unit::Month,
        "w" | "week" | "weeks" => Unit::Week,
        "d" | "day" | "days" => Unit::Day,
        "h" | "hour" | "hours" => Unit::Hour,
        "m" | "min" | "mins" | "minute" | "minutes" => Unit::Minute,
        "s" | "sec" | "secs" | "second" | "seconds" => Unit::Second,
        _ => return None,
    })
}

fn span(unit: Unit, n: i64) -> Span {
    match unit {
        Unit::Year => Span::new().years(n),
        Unit::Quarter => Span::new().months(n * 3),
        Unit::Month => Span::new().months(n),
        Unit::Week => Span::new().weeks(n),
        Unit::Day => Span::new().days(n),
        Unit::Hour => Span::new().hours(n),
        Unit::Minute => Span::new().minutes(n),
        Unit::Second => Span::new().seconds(n),
    }
}

/// First day of the calendar unit containing `d` (weeks start on Monday).
fn unit_start(d: Date, unit: Unit) -> Date {
    match unit {
        Unit::Year => d.first_of_year(),
        Unit::Quarter => {
            let m = (d.month() - 1) / 3 * 3 + 1;
            Date::new(d.year(), m, 1).unwrap_or(d)
        }
        Unit::Month => d.first_of_month(),
        Unit::Week => d
            .checked_sub(Span::new().days(i64::from(d.weekday().to_monday_zero_offset())))
            .unwrap_or(d),
        _ => d,
    }
}

fn add_date(d: Date, unit: Unit, n: i64) -> Result<Date, String> {
    d.checked_add(span(unit, n)).map_err(|e| e.to_string())
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];
const WEEKDAYS: [(&str, Weekday); 7] = [
    ("monday", Weekday::Monday),
    ("tuesday", Weekday::Tuesday),
    ("wednesday", Weekday::Wednesday),
    ("thursday", Weekday::Thursday),
    ("friday", Weekday::Friday),
    ("saturday", Weekday::Saturday),
    ("sunday", Weekday::Sunday),
];

/// Dates and date constants, as an interval of Unix seconds. See `docs/SYNTAX.md` for the full
/// list. Calendar units (`thisweek`, `lastmonth`, `prev2years`) use whole calendar periods with
/// weeks starting on Monday; `past…` forms and bare durations (`2days`) are rolling windows ending
/// now.
pub fn parse_date(s: &str, clock: &Clock) -> Result<Interval, String> {
    let lower = s.trim().to_ascii_lowercase();
    let today = clock.today();
    let next_day = |d: Date| add_date(d, Unit::Day, 1);
    match lower.as_str() {
        "today" => return clock.day_range(today, next_day(today)?),
        "yesterday" => {
            let y = add_date(today, Unit::Day, -1)?;
            return clock.day_range(y, today);
        }
        "tomorrow" => {
            let t = next_day(today)?;
            return clock.day_range(t, next_day(t)?);
        }
        "now" => return Ok(Interval::point(clock.now_secs())),
        "mtd" | "ytd" | "qtd" => {
            let unit = match lower.as_str() {
                "mtd" => Unit::Month,
                "ytd" => Unit::Year,
                _ => Unit::Quarter,
            };
            return Ok(Interval {
                start: clock.midnight(unit_start(today, unit))?,
                end: clock.now_secs() + 1,
            });
        }
        _ => {}
    }
    // Month names (this year’s month) and weekday names (the most recent such day, today
    // included).
    for (i, name) in MONTHS.iter().enumerate() {
        if lower == *name || lower == name[..3] || (i == 8 && lower == "sept") {
            let m = i8::try_from(i + 1).unwrap_or(1);
            let start = Date::new(today.year(), m, 1).map_err(|e| e.to_string())?;
            return clock.day_range(start, add_date(start, Unit::Month, 1)?);
        }
    }
    for (name, wd) in WEEKDAYS {
        if lower == name || lower == name[..3] {
            let back = (today.weekday().to_monday_zero_offset() - wd.to_monday_zero_offset())
                .rem_euclid(7);
            let d = add_date(today, Unit::Day, -i64::from(back))?;
            return clock.day_range(d, next_day(d)?);
        }
    }
    if let Some(iv) = relative(&lower, clock)? {
        return Ok(iv);
    }
    absolute(&lower, clock)
}

/// `this|current|last|prev|previous|past|next|coming` + optional count + unit, or a bare
/// `<count><unit>` duration.
fn relative(s: &str, clock: &Clock) -> Result<Option<Interval>, String> {
    let prefixes = [
        "current", "this", "previous", "prev", "last", "past", "coming", "next",
    ];
    let (prefix, rest) = match prefixes.iter().find(|p| s.starts_with(**p)) {
        Some(p) => (*p, &s[p.len()..]),
        None => ("", s),
    };
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (count, unit) = rest.split_at(digits);
    let Some(unit) = parse_unit(unit) else {
        return Ok(None);
    };
    if prefix.is_empty() && count.is_empty() {
        return Ok(None);
    }
    let n: i64 = if count.is_empty() {
        1
    } else {
        count.parse().map_err(|_| format!("bad count in {s:?}"))?
    };
    let today = clock.today();
    let now = clock.now_secs();
    let rolling = |n: i64| -> Result<Interval, String> {
        let start = clock
            .now
            .checked_sub(span(unit, n))
            .map_err(|e| e.to_string())?;
        Ok(Interval {
            start: i128::from(start.timestamp().as_second()),
            end: now + 1,
        })
    };
    let sub_day = matches!(unit, Unit::Hour | Unit::Minute | Unit::Second);
    let iv = match prefix {
        "" | "past" => rolling(n)?,
        _ if sub_day => match prefix {
            "next" | "coming" => {
                let end = clock
                    .now
                    .checked_add(span(unit, n))
                    .map_err(|e| e.to_string())?;
                Interval {
                    start: now,
                    end: i128::from(end.timestamp().as_second()) + 1,
                }
            }
            _ => rolling(n)?,
        },
        "this" | "current" => {
            let start = unit_start(today, unit);
            clock.day_range(start, add_date(start, unit, 1)?)?
        }
        "last" | "prev" | "previous" => {
            let cur = unit_start(today, unit);
            clock.day_range(add_date(cur, unit, -n)?, cur)?
        }
        _ => {
            let next = add_date(unit_start(today, unit), unit, 1)?;
            clock.day_range(next, add_date(next, unit, n)?)?
        }
    };
    Ok(Some(iv))
}

/// ISO-8601 style dates: `2024`, `2024-05`, `2024-05-17`, `2024-05-17T10`, `2024-05-17T10:30`,
/// `2024-05-17T10:30:15` (`/` also accepted as the date separator, a space instead of `T`).
fn absolute(s: &str, clock: &Clock) -> Result<Interval, String> {
    let err = || {
        format!("unrecognized date {s:?} (use YYYY[-MM[-DD[THH[:MM[:SS]]]]] or a date constant)")
    };
    let s = s.replace('/', "-");
    let (date_part, time_part) = match s.split_once(['t', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (s.as_str(), None),
    };
    let fields: Vec<&str> = date_part.split('-').collect();
    let num = |f: &str| f.parse::<i64>().map_err(|_| err());
    let year = i16::try_from(num(fields.first().copied().unwrap_or(""))?).map_err(|_| err())?;
    if fields.len() > 3 || fields.iter().any(|f| f.is_empty()) {
        return Err(err());
    }
    let month = fields
        .get(1)
        .map(|f| num(f).and_then(|m| i8::try_from(m).map_err(|_| err())))
        .transpose()?;
    let day = fields
        .get(2)
        .map(|f| num(f).and_then(|d| i8::try_from(d).map_err(|_| err())))
        .transpose()?;
    let start = Date::new(year, month.unwrap_or(1), day.unwrap_or(1)).map_err(|_| err())?;
    let Some(time) = time_part else {
        let end = match (month, day) {
            (None, _) => add_date(start, Unit::Year, 1)?,
            (Some(_), None) => add_date(start, Unit::Month, 1)?,
            (Some(_), Some(_)) => add_date(start, Unit::Day, 1)?,
        };
        return clock.day_range(start, end);
    };
    if day.is_none() {
        return Err(err());
    }
    let tf: Vec<&str> = time.split(':').collect();
    if tf.is_empty() || tf.len() > 3 {
        return Err(err());
    }
    let h = i8::try_from(num(tf[0])?).map_err(|_| err())?;
    let mi = tf.get(1).map(|f| num(f)).transpose()?.unwrap_or(0);
    let se = tf.get(2).map(|f| num(f)).transpose()?.unwrap_or(0);
    let dt = start.at(
        h,
        i8::try_from(mi).map_err(|_| err())?,
        i8::try_from(se).map_err(|_| err())?,
        0,
    );
    let z = dt.to_zoned(clock.tz()).map_err(|_| err())?;
    let width = match tf.len() {
        1 => Span::new().hours(1),
        2 => Span::new().minutes(1),
        _ => Span::new().seconds(1),
    };
    let end = z.checked_add(width).map_err(|e| e.to_string())?;
    Ok(Interval {
        start: i128::from(z.timestamp().as_second()),
        end: i128::from(end.timestamp().as_second()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc_clock() -> Clock {
        // Wednesday 2024-06-12 15:30:00 UTC.
        Clock::at(1_718_206_200, TimeZone::UTC)
    }

    fn ts(s: &str) -> i128 {
        i128::from(s.parse::<jiff::Timestamp>().unwrap().as_second())
    }

    fn date(s: &str) -> Interval {
        parse_date(s, &utc_clock()).unwrap()
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("123").unwrap(), Interval::point(123));
        assert_eq!(parse_size("1kb").unwrap(), Interval::point(1024));
        assert_eq!(parse_size("1.5MB").unwrap(), Interval::point(1_572_864));
        assert_eq!(parse_size("2g").unwrap(), Interval::point(2 << 30));
        assert_eq!(parse_size("empty").unwrap(), Interval::point(0));
        let tiny = parse_size("tiny").unwrap();
        assert_eq!((tiny.start, tiny.end), (1, 10 * 1024 + 1));
        assert!(parse_size("1zb").is_err());
        assert!(parse_size("mb").is_err());
    }

    #[test]
    fn ranges() {
        let r = parse_range(">1mb", &parse_size).unwrap();
        assert!(!r.contains(1_048_576));
        assert!(r.contains(1_048_577));
        let r = parse_range(">=1mb", &parse_size).unwrap();
        assert!(r.contains(1_048_576));
        let r = parse_range("<1kb", &parse_size).unwrap();
        assert!(r.contains(1023) && !r.contains(1024));
        let r = parse_range("<=1kb", &parse_size).unwrap();
        assert!(r.contains(1024) && !r.contains(1025));
        let r = parse_range("10mb..100mb", &parse_size).unwrap();
        assert!(r.contains(10 << 20) && r.contains(100 << 20) && !r.contains((100 << 20) + 1));
        let r = parse_range("..5", &parse_number).unwrap();
        assert!(r.contains(0) && r.contains(5) && !r.contains(6));
        let r = parse_range("large", &parse_size).unwrap();
        assert!(r.contains(2 << 20) && !r.contains(1 << 20) && r.contains(16 << 20));
    }

    #[test]
    fn date_constants() {
        assert_eq!(
            date("today"),
            Interval {
                start: ts("2024-06-12T00:00:00Z"),
                end: ts("2024-06-13T00:00:00Z")
            }
        );
        assert_eq!(date("yesterday").start, ts("2024-06-11T00:00:00Z"));
        assert_eq!(
            date("thisweek"),
            Interval {
                start: ts("2024-06-10T00:00:00Z"),
                end: ts("2024-06-17T00:00:00Z")
            }
        );
        assert_eq!(
            date("lastweek"),
            Interval {
                start: ts("2024-06-03T00:00:00Z"),
                end: ts("2024-06-10T00:00:00Z")
            }
        );
        assert_eq!(date("thismonth").start, ts("2024-06-01T00:00:00Z"));
        assert_eq!(
            date("lastmonth"),
            Interval {
                start: ts("2024-05-01T00:00:00Z"),
                end: ts("2024-06-01T00:00:00Z")
            }
        );
        assert_eq!(date("prev2months").start, ts("2024-04-01T00:00:00Z"));
        assert_eq!(
            date("lastyear"),
            Interval {
                start: ts("2023-01-01T00:00:00Z"),
                end: ts("2024-01-01T00:00:00Z")
            }
        );
        assert_eq!(date("nextmonth").start, ts("2024-07-01T00:00:00Z"));
        assert_eq!(date("pastweek").start, ts("2024-06-05T15:30:00Z"));
        assert_eq!(date("past7days").start, ts("2024-06-05T15:30:00Z"));
        assert_eq!(date("2days").start, ts("2024-06-10T15:30:00Z"));
        assert_eq!(date("last2hours").start, ts("2024-06-12T13:30:00Z"));
        assert_eq!(date("ytd").start, ts("2024-01-01T00:00:00Z"));
        assert_eq!(date("qtd").start, ts("2024-04-01T00:00:00Z"));
        assert_eq!(
            date("march"),
            Interval {
                start: ts("2024-03-01T00:00:00Z"),
                end: ts("2024-04-01T00:00:00Z")
            }
        );
        assert_eq!(date("mar").start, ts("2024-03-01T00:00:00Z"));
        assert_eq!(date("monday").start, ts("2024-06-10T00:00:00Z"));
        assert_eq!(date("wednesday").start, ts("2024-06-12T00:00:00Z"));
        assert_eq!(date("thursday").start, ts("2024-06-06T00:00:00Z"));
    }

    #[test]
    fn absolute_dates() {
        assert_eq!(
            date("2024"),
            Interval {
                start: ts("2024-01-01T00:00:00Z"),
                end: ts("2025-01-01T00:00:00Z")
            }
        );
        assert_eq!(
            date("2024-02"),
            Interval {
                start: ts("2024-02-01T00:00:00Z"),
                end: ts("2024-03-01T00:00:00Z")
            }
        );
        assert_eq!(date("2024/02/29").start, ts("2024-02-29T00:00:00Z"));
        assert_eq!(
            date("2024-02-29T10"),
            Interval {
                start: ts("2024-02-29T10:00:00Z"),
                end: ts("2024-02-29T11:00:00Z")
            }
        );
        assert_eq!(date("2024-02-29T10:30:05").end, ts("2024-02-29T10:30:06Z"));
        assert!(parse_date("2024-13", &utc_clock()).is_err());
        assert!(parse_date("someday", &utc_clock()).is_err());
        let r = parse_range("2024-01..2024-03", &|v| parse_date(v, &utc_clock())).unwrap();
        assert_eq!(r.lo, Some(ts("2024-01-01T00:00:00Z")));
        assert_eq!(r.hi, Some(ts("2024-04-01T00:00:00Z")));
    }

    #[test]
    fn time_zones_matter() {
        let berlin = TimeZone::get("Europe/Berlin").unwrap_or(TimeZone::UTC);
        let c = Clock::at(1_718_206_200, berlin.clone());
        let iv = parse_date("today", &c).unwrap();
        if berlin != TimeZone::UTC {
            assert_eq!(iv.start, ts("2024-06-11T22:00:00Z"));
        }
    }
}
