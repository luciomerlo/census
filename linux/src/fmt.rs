//! Formato humano de tamanos, fechas y tablas simples.

use crate::time;

pub fn human(bytes: u64) -> String {
    const U: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < U.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", U[i])
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn human_opt(bytes: Option<u64>) -> String {
    match bytes {
        Some(v) => human(v),
        None => "-".into(),
    }
}

pub fn percent(part: u64, total: u64) -> String {
    if total == 0 {
        "0.0%".into()
    } else {
        format!("{:.1}%", (part as f64) * 100.0 / (total as f64))
    }
}

pub fn bar(part: u64, total: u64, width: usize) -> String {
    let n = if total == 0 {
        0
    } else {
        ((part as f64 / total as f64) * width as f64).round() as usize
    }
    .min(width);
    let mut s = String::with_capacity(width);
    for i in 0..width {
        s.push(if i < n { '#' } else { '.' });
    }
    s
}

pub fn stamp(ns: i64) -> String {
    if ns <= 0 {
        "?".into()
    } else {
        time::stamp_local(ns)
    }
}

pub fn age(ns: i64) -> String {
    time::age_label(ns)
}

/// Imprime filas alineadas: la primera columna a la izquierda y el resto a la
/// derecha cuando es numerico. No pretende ser una tabla Unicode.
pub fn table(headers: &[&str], rows: &[Vec<String>]) {
    if rows.is_empty() {
        println!("{}", headers.join("  "));
        return;
    }
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
    }
    let line = |cells: &[String]| {
        let mut s = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                s.push_str("  ");
            }
            let w = widths.get(i).copied().unwrap_or(0);
            if i == 0 {
                s.push_str(&format!("{cell:<w$}"));
            } else {
                s.push_str(&format!("{cell:>w$}"));
            }
        }
        println!("{s}");
    };
    line(&headers.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    for row in rows {
        line(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(999), "999 B");
        assert_eq!(human(1024), "1.0 KB");
        assert_eq!(human(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn bars() {
        assert_eq!(bar(0, 100, 10), "..........");
        assert_eq!(bar(50, 100, 10), "#####.....");
        assert_eq!(bar(100, 100, 10), "##########");
    }
}
