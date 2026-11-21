//! Paletas de la interfaz: tres temas, el mismo acento ámbar de la marca.
//!
//! Los nombres coinciden con los de WizEverything (`theme=dark|light|contrast`),
//! así que la elección se puede compartir entre las dos apps.

use eframe::egui::{self, Color32, Stroke};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Dark,
    Light,
    /// Alto contraste: fondo negro, texto blanco, sin grises intermedios.
    Contrast,
}

impl Theme {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "dark" | "oscuro" => Some(Theme::Dark),
            "light" | "claro" => Some(Theme::Light),
            "contrast" | "contraste" | "alto" => Some(Theme::Contrast),
            _ => None,
        }
    }

    /// Valor para `settings.ini` (el mismo que usa WizEverything).
    pub fn key(&self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::Contrast => "contrast",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Theme::Dark => "Oscuro",
            Theme::Light => "Claro",
            Theme::Contrast => "Contraste",
        }
    }

    /// Rota entre los tres temas.
    pub fn next(&self) -> Self {
        match self {
            Theme::Dark => Theme::Light,
            Theme::Light => Theme::Contrast,
            Theme::Contrast => Theme::Dark,
        }
    }

    pub fn bg(&self) -> Color32 {
        match self {
            Theme::Dark => Color32::from_rgb(0x0b, 0x0f, 0x14),
            Theme::Light => Color32::from_rgb(0xf7, 0xf8, 0xfa),
            Theme::Contrast => Color32::BLACK,
        }
    }

    pub fn panel(&self) -> Color32 {
        match self {
            Theme::Dark => Color32::from_rgb(0x0f, 0x14, 0x1a),
            Theme::Light => Color32::WHITE,
            Theme::Contrast => Color32::BLACK,
        }
    }

    pub fn text(&self) -> Color32 {
        match self {
            Theme::Dark => Color32::from_rgb(0xf7, 0xfa, 0xff),
            Theme::Light => Color32::from_rgb(0x0d, 0x13, 0x1a),
            Theme::Contrast => Color32::WHITE,
        }
    }

    /// Texto secundario. En oscuro no baja de ~#B6C2D4: los grises apagados
    /// son lo que hacía ilegible la tabla.
    pub fn muted(&self) -> Color32 {
        match self {
            Theme::Dark => Color32::from_rgb(0xb6, 0xc2, 0xd4),
            Theme::Light => Color32::from_rgb(0x1c, 0x28, 0x36),
            Theme::Contrast => Color32::from_rgb(0xe6, 0xe6, 0xe6),
        }
    }

    pub fn accent(&self) -> Color32 {
        match self {
            Theme::Dark => Color32::from_rgb(0xff, 0xb5, 0x45),
            Theme::Light => Color32::from_rgb(0x7c, 0x3a, 0x0a),
            Theme::Contrast => Color32::from_rgb(0xff, 0xd4, 0x00),
        }
    }

    pub fn row_alt(&self) -> Color32 {
        match self {
            Theme::Dark => Color32::from_rgb(0x10, 0x16, 0x1d),
            Theme::Light => Color32::from_rgb(0xf1, 0xf3, 0xf7),
            Theme::Contrast => Color32::from_rgb(0x14, 0x14, 0x14),
        }
    }

    pub fn selection(&self) -> Color32 {
        match self {
            Theme::Dark => Color32::from_rgb(0x2c, 0x3a, 0x52),
            Theme::Light => Color32::from_rgb(0xff, 0xe8, 0xc2),
            Theme::Contrast => Color32::from_rgb(0x3a, 0x3a, 0x00),
        }
    }

    pub fn border(&self) -> Color32 {
        match self {
            Theme::Dark => Color32::from_rgb(0x2a, 0x35, 0x42),
            Theme::Light => Color32::from_rgb(0xdd, 0xe1, 0xe8),
            Theme::Contrast => Color32::WHITE,
        }
    }

    fn border_width(&self) -> f32 {
        match self {
            Theme::Contrast => 2.0,
            _ => 1.0,
        }
    }

    /// Alto de fila (el contraste necesita más aire).
    pub fn row_h(&self) -> f32 {
        match self {
            Theme::Contrast => 26.0,
            _ => 23.0,
        }
    }

    // Tamaños calcados de WizEverything: cuerpo 13, monoespaciado 11,
    // encabezados 10.5, título 17. El tema de contraste sube dos puntos.
    pub fn font_name(&self) -> f32 {
        match self {
            Theme::Contrast => 15.0,
            _ => 13.0,
        }
    }

    /// Monoespaciado: rutas, tamaños y fechas, como en WizEverything.
    pub fn font_small(&self) -> f32 {
        match self {
            Theme::Contrast => 13.0,
            _ => 11.0,
        }
    }

    pub fn font_header(&self) -> f32 {
        match self {
            Theme::Contrast => 12.0,
            _ => 10.5,
        }
    }

    pub fn font_title(&self) -> f32 {
        match self {
            Theme::Contrast => 19.0,
            _ => 17.0,
        }
    }

    pub fn font_status(&self) -> f32 {
        match self {
            Theme::Contrast => 12.5,
            _ => 10.5,
        }
    }

    /// La tipografía de WizEverything: Segoe UI como principal y Segoe UI
    /// Symbol de respaldo para los glifos que Segoe UI no trae.
    ///
    /// Se leen de `%WINDIR%\Fonts` en vez de empaquetarlas: son fuentes del
    /// sistema y así el binario sigue siendo MIT sin redistribuir nada.
    pub fn setup_fonts(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        let windows = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
        let dir = std::path::PathBuf::from(windows).join("Fonts");
        let mut primary: Vec<String> = Vec::new();
        let mut fallback: Vec<String> = Vec::new();
        for (name, file, is_primary) in [
            ("segoe", "segoeui.ttf", true),
            ("segsym", "seguisym.ttf", false),
        ] {
            if let Ok(bytes) = std::fs::read(dir.join(file)) {
                fonts.font_data.insert(
                    name.into(),
                    std::sync::Arc::new(egui::FontData::from_owned(bytes)),
                );
                if is_primary {
                    primary.push(name.into())
                } else {
                    fallback.push(name.into())
                }
            }
        }
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let list = fonts.families.entry(fam.clone()).or_default();
            if fam == egui::FontFamily::Proportional {
                for (i, p) in primary.iter().enumerate() {
                    list.insert(i, p.clone());
                }
            }
            for f in &fallback {
                list.push(f.clone());
            }
        }
        ctx.set_fonts(fonts);
    }

    /// Aplica la paleta a egui.
    pub fn apply(&self, ctx: &egui::Context) {
        let mut visuals = match self {
            Theme::Dark => egui::Visuals::dark(),
            Theme::Light => egui::Visuals::light(),
            Theme::Contrast => egui::Visuals::dark(),
        };
        visuals.panel_fill = self.bg();
        visuals.window_fill = self.panel();
        visuals.extreme_bg_color = self.panel();
        visuals.faint_bg_color = self.row_alt();
        visuals.override_text_color = Some(self.text());
        visuals.selection.bg_fill = self.selection();
        visuals.selection.stroke = Stroke::new(self.border_width(), self.accent());
        visuals.hyperlink_color = self.accent();
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(self.border_width(), self.border());
        visuals.widgets.inactive.bg_stroke = Stroke::new(self.border_width(), self.border());
        visuals.widgets.hovered.bg_stroke = Stroke::new(self.border_width(), self.accent());
        visuals.widgets.active.bg_stroke = Stroke::new(self.border_width() + 0.5, self.accent());
        ctx.set_visuals(visuals);

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        ctx.set_style(style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luma(c: Color32) -> f32 {
        0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32
    }

    fn contrast_ratio(a: Color32, b: Color32) -> f32 {
        let (la, lb) = (luma(a) / 255.0, luma(b) / 255.0);
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn themes_parse_from_cli_and_settings() {
        assert_eq!(Theme::parse("dark"), Some(Theme::Dark));
        assert_eq!(Theme::parse("CLARO"), Some(Theme::Light));
        assert_eq!(Theme::parse("contrast"), Some(Theme::Contrast));
        assert_eq!(Theme::parse("azul"), None);
        assert_eq!(Theme::Contrast.key(), "contrast");
        assert_eq!(Theme::Dark.next(), Theme::Light);
        assert_eq!(Theme::Light.next(), Theme::Contrast);
        assert_eq!(Theme::Contrast.next(), Theme::Dark);
    }

    /// Lo que pedía el usuario: el tema oscuro tiene que ser legible.
    #[test]
    fn every_theme_is_readable() {
        for t in [Theme::Dark, Theme::Light, Theme::Contrast] {
            let body = contrast_ratio(t.text(), t.bg());
            let secondary = contrast_ratio(t.muted(), t.bg());
            let alt = contrast_ratio(t.text(), t.row_alt());
            assert!(body >= 7.0, "{:?}: texto/fondo {body:.1} < 7", t);
            assert!(
                secondary >= 4.5,
                "{:?}: secundario/fondo {secondary:.1} < 4.5",
                t
            );
            assert!(alt >= 7.0, "{:?}: texto/fila alterna {alt:.1} < 7", t);
        }
    }

    #[test]
    fn accent_is_visible_on_every_background() {
        for t in [Theme::Dark, Theme::Light, Theme::Contrast] {
            let r = contrast_ratio(t.accent(), t.bg());
            assert!(r >= 3.0, "{:?}: acento/fondo {r:.1} < 3", t);
        }
    }

    #[test]
    fn contrast_theme_is_the_most_legible() {
        let dark = contrast_ratio(Theme::Dark.muted(), Theme::Dark.bg());
        let contrast = contrast_ratio(Theme::Contrast.muted(), Theme::Contrast.bg());
        assert!(
            contrast > dark,
            "contraste ({contrast:.1}) debería superar al oscuro ({dark:.1})"
        );
    }
}
