//! Nombre del dispositivo en el catalogo.
//!
//! Por defecto es el que le da el sistema operativo (en Android,
//! `Settings.Global.DEVICE_NAME`; en Linux/mac, el hostname). El usuario puede
//! reemplazarlo por uno propio, **sin espacios**, porque el nombre viaja en
//! URLs, rutas del catalogo y nombres DNS de Tailscale.

pub const MAX_LEN: usize = 63;

/// Valida un nombre propio. Acepta letras y numeros (Unicode), `-`, `_` y `.`.
pub fn validate(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("el nombre no puede estar vacio".into());
    }
    if name.chars().any(char::is_whitespace) {
        return Err(format!(
            "el nombre no puede tener espacios; sugerido: {}",
            suggest(name)
        ));
    }
    if name.chars().count() > MAX_LEN {
        return Err(format!("maximo {MAX_LEN} caracteres"));
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(format!("caracter no permitido: {c:?}"));
    }
    Ok(name.to_string())
}

/// Convierte cualquier nombre en uno valido (espacios -> `-`, descarta lo no
/// permitido). Nunca devuelve vacio.
pub fn suggest(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.trim().chars() {
        if c.is_whitespace() {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
            out.push(c);
        }
    }
    let out: String = out.trim_matches('-').chars().take(MAX_LEN).collect();
    if out.is_empty() {
        "dispositivo".into()
    } else {
        out
    }
}

/// Nombre efectivo: el propio si es valido; si no, el del sistema tal cual.
pub fn effective(custom: &str, os_name: &str) -> String {
    match validate(custom) {
        Ok(n) => n,
        Err(_) if !os_name.trim().is_empty() => os_name.trim().to_string(),
        Err(_) => "dispositivo".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_spaces_and_suggests_a_fix() {
        let e = validate("Pixel 8 de Lucio").unwrap_err();
        assert!(e.contains("Pixel-8-de-Lucio"), "{e}");
        assert_eq!(validate("pixel8-lucio").unwrap(), "pixel8-lucio");
        assert_eq!(validate("  tel_1.casa ").unwrap(), "tel_1.casa");
    }

    #[test]
    fn rejects_empty_symbols_and_long() {
        assert!(validate("   ").is_err());
        assert!(validate("a/b").is_err());
        assert!(validate(&"x".repeat(64)).is_err());
        assert!(validate("ñandú").is_ok());
    }

    #[test]
    fn default_is_the_os_name_untouched() {
        assert_eq!(effective("", "Pixel 8 de Lucio"), "Pixel 8 de Lucio");
        assert_eq!(effective("tel-casa", "Pixel 8"), "tel-casa");
        assert_eq!(effective("con espacio", "Pixel 8"), "Pixel 8");
        assert_eq!(effective("", ""), "dispositivo");
    }

    #[test]
    fn suggest_never_returns_empty() {
        assert_eq!(suggest("  !!! "), "dispositivo");
        assert_eq!(suggest("Mi  Tel / 2"), "Mi-Tel-2");
    }
}
