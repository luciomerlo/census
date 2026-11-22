//! Tiempo: nanosegundos desde la epoch, formato local y parseo de fechas.
//!
//! Las fechas del indice se guardan en nanosegundos. Para mostrar y para
//! `dm:today` se usa la hora local; en Unix es la del sistema (libc), en otras
//! plataformas se cae a UTC para que el codigo siga siendo portable y testeable.

use std::time::{SystemTime, UNIX_EPOCH};

pub const NS: i64 = 1_000_000_000;

pub fn now_ns() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos().min(i64::MAX as u128) as i64,
        Err(e) => -(e.duration().as_nanos().min(i64::MAX as u128) as i64),
    }
}

pub fn to_ns(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos().min(i64::MAX as u128) as i64,
        Err(e) => -(e.duration().as_nanos().min(i64::MAX as u128) as i64),
    }
}

pub fn secs(ns: i64) -> i64 {
    ns.div_euclid(NS)
}

pub fn from_secs(s: i64) -> i64 {
    s.saturating_mul(NS)
}

/// Fecha civil (Howard Hinnant) a dias desde 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn format_utc(ns: i64) -> String {
    let s = secs(ns);
    let days = s.div_euclid(86_400);
    let sod = s.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        sod / 3600,
        (sod / 60) % 60,
        sod % 60
    )
}

#[cfg(unix)]
mod local {
    use super::*;
    use std::mem::MaybeUninit;

    pub fn tm_of(ns: i64) -> libc::tm {
        let t = secs(ns) as libc::time_t;
        let mut tm = MaybeUninit::<libc::tm>::uninit();
        unsafe {
            // localtime_r no asigna: el tm vive en el stack.
            libc::localtime_r(&t, tm.as_mut_ptr());
            tm.assume_init()
        }
    }

    pub fn mktime(tm: &mut libc::tm) -> i64 {
        unsafe { libc::mktime(tm) as i64 }
    }

    pub fn format(ns: i64) -> String {
        let tm = tm_of(ns);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }

    pub fn day_bounds(ns: i64) -> (i64, i64) {
        let mut tm = tm_of(ns);
        tm.tm_hour = 0;
        tm.tm_min = 0;
        tm.tm_sec = 0;
        tm.tm_isdst = -1;
        let start = mktime(&mut tm);
        tm.tm_mday += 1;
        tm.tm_hour = 0;
        tm.tm_min = 0;
        tm.tm_sec = 0;
        tm.tm_isdst = -1;
        let end = mktime(&mut tm);
        (from_secs(start), from_secs(end))
    }

    pub fn parse_local(y: i64, mo: u32, d: u32, h: u32, mi: u32, se: u32) -> Option<i64> {
        let mut tm = MaybeUninit::<libc::tm>::zeroed();
        let mut tm = unsafe { tm.assume_init() };
        tm.tm_year = (y - 1900) as libc::c_int;
        tm.tm_mon = (mo as i32 - 1) as libc::c_int;
        tm.tm_mday = d as libc::c_int;
        tm.tm_hour = h as libc::c_int;
        tm.tm_min = mi as libc::c_int;
        tm.tm_sec = se as libc::c_int;
        tm.tm_isdst = -1;
        let t = mktime(&mut tm);
        if t == -1 {
            return None;
        }
        Some(from_secs(t))
    }
}

#[cfg(not(unix))]
mod local {
    use super::*;

    pub fn format(ns: i64) -> String {
        format_utc(ns)
    }

    pub fn day_bounds(ns: i64) -> (i64, i64) {
        let s = secs(ns);
        let start = s.div_euclid(86_400) * 86_400;
        (from_secs(start), from_secs(start + 86_400))
    }

    pub fn parse_local(y: i64, mo: u32, d: u32, h: u32, mi: u32, se: u32) -> Option<i64> {
        let days = days_from_civil(y, mo, d);
        Some(from_secs(days * 86_400 + h as i64 * 3600 + mi as i64 * 60 + se as i64))
    }
}

pub fn stamp_local(ns: i64) -> String {
    local::format(ns)
}

pub fn day_bounds(ns: i64) -> (i64, i64) {
    local::day_bounds(ns)
}

pub fn today_bounds() -> (i64, i64) {
    local::day_bounds(now_ns())
}

pub fn yesterday_bounds() -> (i64, i64) {
    let (a, _) = local::day_bounds(now_ns());
    local::day_bounds(a - NS)
}

/// Parsea una fecha escrita por una persona. Acepta:
/// `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM[:SS]`, `YYYY-MM-DD HH:MM[:SS]`,
/// `now`, `7d`, `12h`, `30m`, `2w` y `@<epoch>`.
pub fn parse_when(raw: &str) -> Option<i64> {
    let s = raw.trim().trim_matches('"');
    if s.is_empty() {
        return None;
    }
    if s.eq_ignore_ascii_case("now") || s.eq_ignore_ascii_case("hoy") {
        return Some(now_ns());
    }
    if let Some(epoch) = s.strip_prefix('@') {
        return epoch.parse::<i64>().ok().map(|v| from_secs(v));
    }
    // Relativo: 7d, 12h, 30m, 2w, 45s.
    if let Some(rest) = s.strip_suffix(['d', 'D', 'h', 'H', 'm', 'M', 'w', 'W', 's', 'S']) {
        if let Ok(n) = rest.parse::<i64>() {
            let unit = s.chars().last()?.to_ascii_lowercase();
            let delta = match unit {
                'd' => 86_400,
                'w' => 7 * 86_400,
                'h' => 3600,
                'm' => 60,
                _ => 1,
            };
            return Some(now_ns().saturating_sub(from_secs(n.saturating_mul(delta))));
        }
    }
    let (date, time) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: u32 = dp.next()?.parse().ok()?;
    let d: u32 = dp.next()?.parse().ok()?;
    if dp.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let (h, mi, se) = match time {
        Some(t) => {
            let mut tp = t.trim_end_matches('Z').split(':');
            let h: u32 = tp.next()?.parse().ok()?;
            let mi: u32 = tp.next().unwrap_or("0").parse().ok()?;
            let se: u32 = tp.next().unwrap_or("0").parse().ok()?;
            if h > 23 || mi > 59 || se > 60 {
                return None;
            }
            (h, mi, se)
        }
        None => (0, 0, 0),
    };
    local::parse_local(y, mo, d, h, mi, se)
}

pub fn parse_range(raw: &str) -> Option<(i64, i64)> {
    if raw.eq_ignore_ascii_case("today") || raw.eq_ignore_ascii_case("hoy") {
        return Some(today_bounds());
    }
    if raw.eq_ignore_ascii_case("yesterday") || raw.eq_ignore_ascii_case("ayer") {
        return Some(yesterday_bounds());
    }
    let t = parse_when(raw)?;
    // Una fecha sin hora significa "ese dia"; con hora, desde ese instante.
    if !raw.contains(':') && !raw.contains('T') {
        Some(day_bounds(t))
    } else {
        Some((t, i64::MAX))
    }
}

pub fn age_label(ns: i64) -> String {
    if ns <= 0 {
        return "?".into();
    }
    let d = now_ns().saturating_sub(ns);
    let s = secs(d.max(0));
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrip() {
        for days in [0, 1, -1, 19_000, 20_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn utc_format() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00");
        assert_eq!(format_utc(1_700_000_000 * NS), "2023-11-14 22:13:20");
    }

    #[test]
    fn parses_dates_and_relatives() {
        assert!(parse_when("2024-01-31").is_some());
        assert!(parse_when("2024-01-31T10:20:30").is_some());
        assert!(parse_when("7d").unwrap() < now_ns());
        assert!(parse_when("nada").is_none());
        assert!(parse_when("@1700000000").is_some());
    }

    #[test]
    fn day_bounds_are_ordered() {
        let (a, b) = today_bounds();
        assert!(a < b);
        assert_eq!(secs(b) - secs(a), 86_400);
    }
}
