//! Conversión entre FILETIME (100 ns desde 1601), segundos Unix y fechas
//! locales. Todo con `jiff`, que ya viene con `mft`: sin `chrono`.

pub const FILETIME_UNIX_DIFF: i64 = 11_644_473_600;

pub fn unix_to_filetime(secs: i64) -> u64 {
    let v = secs.saturating_add(FILETIME_UNIX_DIFF);
    (v.max(0) as u64).saturating_mul(10_000_000)
}

pub fn filetime_to_unix(ft: u64) -> i64 {
    (ft / 10_000_000) as i64 - FILETIME_UNIX_DIFF
}

// ------------------------------------------------------------------- fechas

/// Segundos Unix de ahora.
pub fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

fn tz() -> jiff::tz::TimeZone {
    jiff::tz::TimeZone::system()
}

/// `unix` a hora local con el formato pedido (strftime). Vacío si no se puede.
pub fn format_local(unix: i64, fmt: &str) -> String {
    match jiff::Timestamp::from_second(unix) {
        Ok(t) => t.to_zoned(tz()).strftime(fmt).to_string(),
        Err(_) => String::new(),
    }
}

/// Formato que usan la tabla y el MCP: `2026-11-21 09:54`.
pub fn stamp_local(unix: i64) -> String {
    format_local(unix, "%Y-%m-%d %H:%M")
}

/// Día local (00:00 a 00:00 del siguiente) que contiene a `unix`.
pub fn day_bounds(unix: i64) -> Option<(i64, i64)> {
    let zoned = jiff::Timestamp::from_second(unix).ok()?.to_zoned(tz());
    let start = zoned.start_of_day().ok()?;
    let end = start.tomorrow().ok()?.start_of_day().ok()?;
    Some((start.timestamp().as_second(), end.timestamp().as_second()))
}

/// Día local de hoy y de ayer, en segundos Unix.
pub fn today_bounds() -> Option<(i64, i64)> {
    day_bounds(now())
}

pub fn yesterday_bounds() -> Option<(i64, i64)> {
    let ayer = jiff::Zoned::now().yesterday().ok()?;
    day_bounds(ayer.timestamp().as_second())
}

/// Interpreta `2026-01-31T10:00:00` y parientes como hora local.
pub fn parse_local_stamp(s: &str) -> Option<i64> {
    const DTS: [&str; 5] = [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y/%m/%d %H:%M:%S",
    ];
    for f in DTS {
        if let Ok(dt) = jiff::civil::DateTime::strptime(f, s) {
            if let Ok(z) = dt.to_zoned(tz()) {
                return Some(z.timestamp().as_second());
            }
        }
    }
    for f in ["%Y-%m-%d", "%Y/%m/%d"] {
        if let Ok(d) = jiff::civil::Date::strptime(f, s) {
            if let Ok(z) = d.to_zoned(tz()) {
                return day_bounds(z.timestamp().as_second()).map(|(a, _)| a);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip_in_local_time() {
        // Un instante cualquiera: formatear y volver a parsear da lo mismo.
        let t = 1_800_000_000i64;
        let s = stamp_local(t);
        assert_eq!(s.len(), 16, "formato inesperado: {s}");
        let back = parse_local_stamp(&s).expect("se parsea lo que formateamos");
        assert!((back - t).abs() <= 60, "{back} vs {t}");
        // Fechas sueltas: el inicio del día local.
        let (a, b) = day_bounds(t).unwrap();
        assert_eq!(b - a, 86_400, "sin DST, un día son 86400 s");
        assert!(a <= t && t < b);
        assert_eq!(parse_local_stamp(&s[..10]).unwrap(), a);
        // Basura: no panic, None.
        assert!(parse_local_stamp("no es fecha").is_none());
        assert!(parse_local_stamp("").is_none());
        assert!(day_bounds(i64::MAX).is_none());
    }

    #[test]
    fn epoch_maps_to_the_known_constant() {
        assert_eq!(unix_to_filetime(0), 116_444_736_000_000_000);
        assert_eq!(filetime_to_unix(116_444_736_000_000_000), 0);
    }

    #[test]
    fn roundtrip() {
        for t in [1_600_000_000i64, 1_700_000_000, 0, -86_400] {
            assert_eq!(filetime_to_unix(unix_to_filetime(t)), t);
        }
    }

    #[test]
    fn matches_what_sorteverything_expects() {
        // SortEverything hace ticks/10^7 - 11644473600 (truncando).
        let t = 1_700_000_123i64;
        let ft = unix_to_filetime(t);
        assert_eq!((ft / 10_000_000) as i64 - 11_644_473_600, t);
    }
}
