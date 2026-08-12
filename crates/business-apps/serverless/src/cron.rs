//! Minimal cron engine (pure std, no chrono): 5-field schedules
//! (`min hour dom month dow` with `*`, `*/n`, `a-b`, `a,b`) plus
//! `@every <N><s|m|h|d>` intervals. `next_run` computes the next firing
//! strictly after the given Unix-epoch millis — ported from zw's `cron.rs`.

/// `unix_millis` → `(year, month 1-12, day 1-31, hour, minute, weekday 0-6 Sun=0)`.
fn parts(millis: i64) -> (i64, i64, i64, i64, i64, i64) {
    let secs = millis.div_euclid(1000);
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let hour = tod / 3600;
    let minute = (tod % 3600) / 60;

    // Civil-from-days (Howard Hinnant): days since 1970-01-01 → y/m/d.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = if month <= 2 { year + 1 } else { year };
    let weekday = (days + 4).rem_euclid(7);
    (year, month, day, hour, minute, weekday)
}

/// Days since 1970-01-01 for a date (Hinnant's days-from-civil).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

struct Field {
    values: std::collections::BTreeSet<i64>,
    star: bool,
}

impl Field {
    fn all() -> Field {
        Field { values: std::collections::BTreeSet::new(), star: true }
    }

    fn matches(&self, v: i64) -> bool {
        self.star || self.values.contains(&v)
    }
}

fn parse_field(s: &str, lo: i64, hi: i64) -> Option<Field> {
    if s == "*" {
        return Some(Field::all());
    }
    let mut values = std::collections::BTreeSet::new();
    for piece in s.split(',') {
        if piece.is_empty() {
            return None;
        }
        let (range, step) = match piece.split_once('/') {
            Some((r, st)) => (r, st.parse::<i64>().ok()?),
            None => (piece, 1),
        };
        if step < 1 {
            return None;
        }
        if range == "*" {
            let mut v = lo;
            while v <= hi {
                values.insert(v);
                v += step;
            }
        } else if let Some((a, b)) = range.split_once('-') {
            let (a, b) = (a.parse::<i64>().ok()?, b.parse::<i64>().ok()?);
            if a < lo || b > hi || a > b {
                return None;
            }
            let mut v = a;
            while v <= b {
                values.insert(v);
                v += step;
            }
        } else {
            let v: i64 = piece.parse().ok()?;
            if v < lo || v > hi {
                return None;
            }
            values.insert(v);
        }
    }
    if values.is_empty() {
        return None;
    }
    Some(Field { values, star: false })
}

fn every_seconds(rest: &str) -> Option<i64> {
    let mut tokens = rest.split_whitespace();
    let first = tokens.next()?;
    let (num, unit) = match tokens.next() {
        Some(u) => (first, u),
        None => {
            let idx = first.find(|c: char| !c.is_ascii_digit()).unwrap_or(first.len());
            if idx == 0 {
                return None;
            }
            let (n, u) = first.split_at(idx);
            (n, u)
        }
    };
    let n: i64 = num.parse().ok()?;
    let secs = match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => n,
        "m" | "min" | "mins" | "minute" | "minutes" => n * 60,
        "h" | "hr" | "hour" | "hours" => n * 3600,
        "d" | "day" | "days" => n * 86400,
        _ => return None,
    };
    (secs > 0).then_some(secs)
}

/// The next firing strictly after `after_millis`, in Unix-epoch millis.
/// Returns None for invalid schedules or when the next run would be more
/// than 4 years out.
pub fn next_run(schedule: &str, after_millis: i64) -> Option<i64> {
    let s = schedule.trim();
    if let Some(rest) = s.strip_prefix("@every") {
        let secs = every_seconds(rest.trim())?;
        return Some(after_millis + secs * 1000);
    }
    let fields: Vec<&str> = s.split_whitespace().collect();
    if fields.len() != 5 {
        return None;
    }
    let min = parse_field(fields[0], 0, 59)?;
    let hour = parse_field(fields[1], 0, 23)?;
    let dom = parse_field(fields[2], 1, 31)?;
    let month = parse_field(fields[3], 1, 12)?;
    let mut dow = parse_field(fields[4], 0, 7)?;
    if dow.values.contains(&7) {
        dow.values.insert(0); // 7 == Sunday alias of 0
    }

    let (mut year, mut month_n, mut day, mut h, mut m, mut wd) = parts(after_millis);

    // Scan minute by minute; skip the current minute except for its
    // remainder? We search strictly after `after_millis`, so start at the
    // next minute boundary.
    let mut minute_of_day = h * 60 + m + 1; // first candidate
    for _ in 0..(365 * 24 * 60 * 4) {
        while minute_of_day < 1440 {
            let hh = minute_of_day / 60;
            let mm = minute_of_day % 60;
            if min.matches(mm)
                && hour.matches(hh)
                && dom.matches(day)
                && month.matches(month_n)
                && dow.matches(wd)
            {
                let days = days_from_civil(year, month_n, day);
                return Some((days * 86400 + hh * 3600 + mm * 60) * 1000);
            }
            minute_of_day += 1;
        }
        // next day
        minute_of_day = 0;
        day += 1;
        wd = (wd + 1) % 7;
        if day > days_in_month(year, month_n) {
            day = 1;
            month_n += 1;
            if month_n > 12 {
                month_n = 1;
                year += 1;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iso(millis: i64) -> String {
        let (y, mo, d, h, mi, _) = parts(millis);
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")
    }

    #[test]
    fn every_interval() {
        let now = 1_700_000_000_000;
        assert_eq!(next_run("@every 30s", now), Some(now + 30_000));
        assert_eq!(next_run("@every 5m", now), Some(now + 300_000));
        assert_eq!(next_run("@every 2h", now), Some(now + 7_200_000));
        assert_eq!(next_run("@every 0s", now), None);
    }

    #[test]
    fn five_field_daily() {
        // 2023-11-14 22:13:20 UTC → next "0 5 * * *" is 2023-11-15 05:00.
        let now = 1_700_000_000_000;
        let n = next_run("0 5 * * *", now).unwrap();
        assert_eq!(iso(n), "2023-11-15 05:00");
    }

    #[test]
    fn five_field_weekday() {
        // 2023-11-14 is a Tuesday. "30 9 * * 1-5" → 2023-11-15 09:30.
        let now = 1_700_000_000_000;
        let n = next_run("30 9 * * 1-5", now).unwrap();
        assert_eq!(iso(n), "2023-11-15 09:30");
    }

    #[test]
    fn invalid_schedules() {
        assert_eq!(next_run("bogus", 0), None);
        assert_eq!(next_run("60 * * * *", 0), None);
        assert_eq!(next_run("", 0), None);
        assert_eq!(next_run("* * * *", 0), None);
    }

    #[test]
    fn minutely_lands_next_minute() {
        let now = 1_700_000_000_000; // 22:13:20
        let n = next_run("* * * * *", now).unwrap();
        assert_eq!(iso(n), "2023-11-14 22:14");
    }
}