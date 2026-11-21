//! Ventana de Census: barra de búsqueda arriba, tabla virtualizada, barra de
//! estado abajo. Dos temas y atajos de teclado.
//!
//! Todo se dibuja con `painter` y `show_rows`: se pintan sólo las filas
//! visibles, así 1,5 millones de resultados no cuestan nada por frame.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, FontId, Key, Rect, RichText, Sense, Vec2};

use crate::index::F_DIR;
use crate::query::{self, Sort};
use crate::theme::Theme;
use crate::Index;

/// Tope de coincidencias que la ventana ordena y muestra.
const CAP: usize = 200_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Name,
    Path,
    Size,
    Modified,
}

pub struct GuiOptions {
    pub theme: Theme,
    pub initial_query: String,
    pub smoke_frames: Option<u32>,
}

pub struct GuiApp {
    index: Arc<Index>,
    query: String,
    applied_query: String,
    sort: Sort,
    theme: Theme,
    applied_theme: Option<Theme>,
    ids: Vec<u32>,
    truncated: bool,
    elapsed_ms: f64,
    selected: Option<u32>,
    focus_search: bool,
    smoke: Option<u32>,
    fonts_ready: bool,
    show_drives: bool,
    excluded: Vec<String>,
    drives_dirty: bool,
    /// Búsqueda en lenguaje natural con Groq.
    ai_mode: bool,
    ai_asking: bool,
    ai_rx: Option<std::sync::mpsc::Receiver<Result<(String, String), String>>>,
    ai_answer: Option<String>,
    ai_model: Option<String>,
    ai_error: Option<String>,
    ai_question: String,
    key_dialog: bool,
    key_input: String,
}

fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    Some(base.join("Census").join("settings.ini"))
}

fn load_theme() -> Option<Theme> {
    let p = settings_path()?;
    let text = std::fs::read_to_string(p).ok()?;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("theme=") {
            return Theme::parse(v.trim());
        }
    }
    None
}

fn save_theme(theme: Theme) {
    let Some(p) = settings_path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, format!("theme={}\n", theme.key()));
}

impl GuiApp {
    pub fn new(index: Arc<Index>, opts: GuiOptions) -> Self {
        let theme = if opts.theme == Theme::Dark {
            load_theme().unwrap_or(opts.theme)
        } else {
            opts.theme
        };
        Self {
            index,
            query: opts.initial_query,
            applied_query: String::from("\u{0}no-ejecutada"),
            sort: Sort::NameAsc,
            theme,
            applied_theme: None,
            ids: Vec::new(),
            truncated: false,
            elapsed_ms: 0.0,
            selected: None,
            focus_search: true,
            smoke: opts.smoke_frames,
            fonts_ready: false,
            show_drives: false,
            excluded: crate::volumes::excluded_drives(),
            drives_dirty: false,
            ai_mode: false,
            ai_asking: false,
            ai_rx: None,
            ai_answer: None,
            ai_model: None,
            ai_error: None,
            ai_question: String::new(),
            key_dialog: false,
            key_input: String::new(),
        }
    }

    fn refresh(&mut self) {
        let t0 = std::time::Instant::now();
        let q = query::parse(&self.query, &self.index);
        let mut ids = self.index.match_ids_capped(&q, CAP);
        self.truncated = ids.len() >= CAP;
        self.index.sort_ids(&mut ids, self.sort);
        self.elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        self.ids = ids;
        self.selected = None;
    }

    fn human(&self, id: u32) -> (String, String, String, String, bool) {
        let n = &self.index.nodes[id as usize];
        let name = self.index.name_string(id);
        let path = self.index.dir_path(id);
        let is_dir = n.flags & F_DIR != 0;
        let size = if is_dir {
            String::from("<dir>")
        } else {
            human_size(n.size)
        };
        let date = chrono::DateTime::from_timestamp(n.mtime, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        (name, path, size, date, is_dir)
    }

    fn open_selected(&self) {
        let Some(id) = self.selected else { return };
        let path = self.index.full_path(id);
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }

    fn header(&self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, self.theme.row_h()), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, self.theme.panel());
        let font = FontId::monospace(self.theme.font_header());
        for (col, label) in [
            (Column::Name, "Nombre"),
            (Column::Path, "Carpeta"),
            (Column::Size, "Tamaño"),
            (Column::Modified, "Modificado"),
        ] {
            let r = self.column_rect(rect, col);
            let align = match col {
                Column::Size => Align2::RIGHT_CENTER,
                _ => Align2::LEFT_CENTER,
            };
            let x = match align {
                Align2::RIGHT_CENTER => r.right(),
                _ => r.left(),
            };
            ui.painter().text(
                egui::pos2(x, rect.center().y),
                align,
                label,
                font.clone(),
                self.theme.muted(),
            );
        }
    }

    fn column_rect(&self, row: Rect, col: Column) -> Rect {
        let w = row.width();
        let pad = 10.0;
        match col {
            Column::Name => Rect::from_min_max(row.left_top() + Vec2::new(pad, 0.0), row.left_top() + Vec2::new(w * 0.40, row.height())),
            Column::Path => Rect::from_min_max(row.left_top() + Vec2::new(w * 0.40 + pad, 0.0), row.left_top() + Vec2::new(w * 0.72, row.height())),
            Column::Size => Rect::from_min_max(row.left_top() + Vec2::new(w * 0.72, 0.0), row.left_top() + Vec2::new(w * 0.835, row.height())),
            Column::Modified => Rect::from_min_max(row.left_top() + Vec2::new(w * 0.84, 0.0), row.right_top() - Vec2::new(pad, 0.0)),
        }
    }

    fn rows(&mut self, ui: &mut egui::Ui) {
        let total = self.ids.len();
        let theme = self.theme;
        let selected = self.selected;
        let mut clicked: Option<u32> = None;
        let mut opened: Option<u32> = None;
        let mut copy: Option<String> = None;

        let row_h = self.theme.row_h();
        let font_name = FontId::proportional(self.theme.font_name());
        let font_small = FontId::monospace(self.theme.font_small());
        let mut draw_row = |ui: &mut egui::Ui, visual_row: usize| {
            let id = self.ids[visual_row];
            let width = ui.available_width();
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::click());
            if !ui.is_rect_visible(rect) {
                return;
            }
            let is_sel = selected == Some(id);
            let bg = if is_sel {
                theme.selection()
            } else if visual_row % 2 == 0 {
                theme.row_alt()
            } else {
                Color32::TRANSPARENT
            };
            if bg != Color32::TRANSPARENT {
                ui.painter().rect_filled(rect, 0.0, bg);
            }
            let (name, path, size, date, is_dir) = self.human(id);

            let name_rect = self.column_rect(rect, Column::Name);
            let icon = if is_dir { "[dir] " } else { "" };
            ui.painter().text(
                egui::pos2(name_rect.left(), rect.center().y),
                Align2::LEFT_CENTER,
                format!("{icon}{name}"),
                font_name.clone(),
                if is_dir { theme.accent() } else { theme.text() },
            );
            ui.painter().text(
                egui::pos2(self.column_rect(rect, Column::Path).left(), rect.center().y),
                Align2::LEFT_CENTER,
                path.as_str(),
                font_small.clone(),
                theme.muted(),
            );
            ui.painter().text(
                egui::pos2(self.column_rect(rect, Column::Size).right(), rect.center().y),
                Align2::RIGHT_CENTER,
                size,
                font_small.clone(),
                theme.muted(),
            );
            ui.painter().text(
                egui::pos2(self.column_rect(rect, Column::Modified).right(), rect.center().y),
                Align2::RIGHT_CENTER,
                date,
                font_small.clone(),
                theme.muted(),
            );

            if resp.clicked() {
                clicked = Some(id);
                if ui.input(|i| i.modifiers.command) {
                    copy = Some(self.index.full_path(id));
                }
            }
            if resp.double_clicked() {
                opened = Some(id);
            }
        };

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_h, total, |ui, range| {
                for i in range {
                    draw_row(ui, i);
                }
            });

        if let Some(id) = clicked {
            self.selected = Some(id);
        }
        if copy.is_some() {
            if let Some(path) = copy {
                ui.ctx().copy_text(path);
            }
        }
        if opened.is_some() {
            self.selected = opened;
            self.open_selected();
        }
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        let theme = self.theme;
        egui::TopBottomPanel::top("census_top")
            .frame(egui::Frame::NONE.fill(theme.panel()).inner_margin(egui::Margin::symmetric(12, 10)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Census").size(theme.font_title()).strong().color(theme.accent()));
                    let vols = self.index.roots.len();
                    let what = if vols == 1 {
                        format!("· unidad {}", self.index.name_string(self.index.root))
                    } else {
                        format!("· {vols} unidades")
                    };
                    ui.label(RichText::new(what).size(theme.font_small()).color(theme.muted()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ai_label = if self.ai_mode { "IA: sí" } else { "IA: no" };
                        if ui
                            .button(ai_label)
                            .on_hover_text("Preguntar en lenguaje natural (Groq + herramientas de Census)")
                            .clicked()
                        {
                            self.ai_mode = !self.ai_mode;
                            if self.ai_mode && !crate::ai::has_key() {
                                self.key_dialog = true;
                            }
                            self.applied_query.clear();
                        }
                        let vols_on = self.index.roots.len();
                        if ui
                            .button(format!("Unidades: {vols_on}"))
                            .on_hover_text("Que unidades indexa Census (igual que WizEverything)")
                            .clicked()
                        {
                            self.show_drives = !self.show_drives;
                        }
                        let label = format!("Tema: {}", theme.name());
                        if ui
                            .button(label)
                            .on_hover_text("Oscuro / Claro / Contraste")
                            .clicked()
                        {
                            self.theme = theme.next();
                            save_theme(self.theme);
                        }
                        let sort_label = match self.sort {
                            Sort::NameAsc => "Nombre A-Z",
                            Sort::NameDesc => "Nombre Z-A",
                        };
                        if ui.button(sort_label).on_hover_text("Invertir el orden").clicked() {
                            self.sort = match self.sort {
                                Sort::NameAsc => Sort::NameDesc,
                                Sort::NameDesc => Sort::NameAsc,
                            };
                            self.applied_query.clear();
                        }
                    });
                });
                ui.add_space(8.0);
                let edit = egui::TextEdit::singleline(&mut self.query)
                    .hint_text("path:\"C:\\Users\"  file:  ext:rs,toml  size:>10mb  dm:today  rc:>=2026-01-01T00:00:00")
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace)
                    .margin(egui::Margin::symmetric(10, 7));
                let resp = ui.add(edit);
                if self.focus_search {
                    resp.request_focus();
                    self.focus_search = false;
                }
                if resp.changed() {
                    self.applied_query.clear();
                }
            });
    }

    /// Selector de unidades: la misma lista de inclusion que WizEverything,
    /// guardada en `%APPDATA%\Census\settings.ini` con su formato.
    fn drives_window(&mut self, ctx: &egui::Context) {
        if !self.show_drives {
            return;
        }
        let theme = self.theme;
        let mut open = true;
        let mut toggled: Option<(String, bool)> = None;
        let roster = crate::volumes::roster();
        egui::Window::new("Unidades")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_min_width(430.0);
                ui.label(
                    RichText::new("Census indexa todas las unidades disponibles: locales por MFT y red o nube recorriendo carpetas. Destilda una para dejarla afuera.")
                        .size(theme.font_small())
                        .color(theme.muted()),
                );
                ui.add_space(8.0);
                for (d, on) in &roster {
                    let mut checked = *on;
                    let name = RichText::new(format!("{}", d.name)).size(theme.font_name()).strong().color(theme.text());
                    let kind = d.kind();
                    let kind_color = if matches!(kind, crate::DriveKind::Cloud) {
                        theme.accent()
                    } else {
                        theme.muted()
                    };
                    let method = if d.uses_mft() { "mft" } else { "recorrido" };
                    let detail = format!(
                        "{}  ·  {}  ·  {}  ·  {} libres",
                        kind.label(),
                        method,
                        if d.fs.is_empty() { "-" } else { &d.fs },
                        human_size(d.free)
                    );
                    ui.horizontal(|ui| {
                        let resp = ui.checkbox(&mut checked, name);
                        ui.label(RichText::new(detail).size(theme.font_small()).color(kind_color));
                        if resp.changed() {
                            toggled = Some((d.name.clone(), checked));
                        }
                    });
                    if !d.label.trim().is_empty() {
                        ui.label(
                            RichText::new(format!("    {}", d.label.trim()))
                                .size(theme.font_small() - 0.5)
                                .color(theme.muted()),
                        );
                    }
                    ui.add_space(4.0);
                }
                if self.drives_dirty {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Guardado. Reinicia Census para indexar el cambio.")
                            .size(theme.font_small())
                            .color(theme.accent()),
                    );
                }
            });
        if let Some((name, checked)) = toggled {
            self.excluded.retain(|e| !e.eq_ignore_ascii_case(&name));
            if !checked {
                self.excluded.push(name);
            }
            match crate::volumes::save_excluded(&self.excluded) {
                Ok(()) => self.drives_dirty = true,
                Err(e) => eprintln!("no se pudo guardar la seleccion de unidades: {e}"),
            }
        }
        if !open {
            self.show_drives = false;
        }
    }

    /// Lanza la consulta en lenguaje natural en un hilo aparte: Groq puede
    /// tardar y la ventana no se puede congelar.
    fn ask_ai(&mut self, ctx: &egui::Context) {
        let question = self.query.trim().to_string();
        if question.is_empty() || self.ai_asking {
            return;
        }
        let Some(key) = crate::ai::load_key() else {
            self.key_dialog = true;
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.ai_rx = Some(rx);
        self.ai_asking = true;
        self.ai_error = None;
        self.ai_question = question.clone();
        let index = Arc::clone(&self.index);
        std::thread::spawn(move || {
            let service = Arc::new(crate::Service::new(Arc::clone(&index)));
            let backend = crate::mcp::Backend::Local { index, service };
            let groq = crate::ai::Groq::new(key);
            let r = crate::ai::ask_auto(&groq, None, &question, &backend, true, &|_m| {});
            let _ = tx.send(r);
        });
        ctx.request_repaint();
    }

    fn poll_ai(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.ai_rx.take() else { return };
        match rx.try_recv() {
            Ok(Ok((model, answer))) => {
                self.ai_model = Some(model);
                self.ai_answer = Some(answer);
                self.ai_error = None;
                self.ai_asking = false;
            }
            Ok(Err(e)) => {
                self.ai_error = Some(e);
                self.ai_asking = false;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.ai_rx = Some(rx);
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.ai_error = Some("el hilo de la IA se cortó".into());
                self.ai_asking = false;
            }
        }
    }

    /// Pedir o borrar la clave de Groq. Se guarda cifrada con DPAPI.
    fn key_dialog(&mut self, ctx: &egui::Context) {
        if !self.key_dialog {
            return;
        }
        let theme = self.theme;
        let mut open = true;
        let mut save = false;
        let mut clear = false;
        egui::Window::new("Clave de Groq")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_min_width(430.0);
                ui.label(
                    RichText::new(
                        "La búsqueda en lenguaje natural usa un modelo de Groq. La clave se guarda cifrada con DPAPI para tu usuario de Windows; también podés definir GROQ_API_KEY.",
                    )
                    .size(theme.font_small())
                    .color(theme.muted()),
                );
                ui.add_space(8.0);
                let edit = egui::TextEdit::singleline(&mut self.key_input)
                    .password(true)
                    .hint_text("gsk_...")
                    .desired_width(f32::INFINITY);
                let resp = ui.add(edit);
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    save = true;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Guardar").clicked() {
                        save = true;
                    }
                    if crate::ai::has_key() && ui.button("Borrar").clicked() {
                        clear = true;
                    }
                    if ui.link("Obtener una clave en console.groq.com").clicked() {
                        ctx.open_url(egui::OpenUrl::new_tab("https://console.groq.com/keys"));
                    }
                });
            });
        if save && !self.key_input.trim().is_empty() {
            match crate::ai::save_key(self.key_input.trim()) {
                Ok(()) => {
                    self.key_input.clear();
                    self.key_dialog = false;
                    self.ai_error = None;
                }
                Err(e) => self.ai_error = Some(e),
            }
        }
        if clear {
            let _ = crate::ai::clear_key();
            self.ai_mode = false;
            self.ai_answer = None;
            self.key_dialog = false;
        }
        if !open {
            self.key_dialog = false;
        }
    }

    /// Panel de respuesta: la pregunta, el modelo y el texto del asistente.
    fn ai_panel(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme;
        if self.ai_question.is_empty() && !self.ai_asking {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("Preguntá con tus palabras")
                        .size(theme.font_title() + 3.0)
                        .color(theme.text()),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Un modelo de Groq responde sobre este mismo índice, con las herramientas de Census.",
                    )
                    .size(theme.font_small())
                    .color(theme.muted()),
                );
                ui.add_space(14.0);
                for ejemplo in [
                    "¿cuánto ocupan los videos de este año?",
                    "los 10 archivos más grandes de Descargas",
                    "¿qué PDFs se tocaron hoy?",
                ] {
                    ui.label(RichText::new(ejemplo).size(theme.font_small()).color(theme.accent()));
                }
            });
            return;
        }

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Pregunta: {}", self.ai_question))
                    .size(theme.font_small())
                    .color(theme.muted()),
            );
            if let Some(m) = &self.ai_model {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(m).size(theme.font_small()).color(theme.accent()));
                });
            }
        });
        ui.add_space(8.0);
        if self.ai_asking {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    RichText::new("Consultando el índice…")
                        .size(theme.font_name())
                        .color(theme.text()),
                );
            });
            return;
        }
        if let Some(e) = &self.ai_error {
            ui.label(RichText::new(e).size(theme.font_small()).color(theme.accent()));
            return;
        }
        if let Some(a) = &self.ai_answer {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add(
                    egui::Label::new(RichText::new(a).size(theme.font_name()).color(theme.text()))
                        .wrap()
                        .selectable(true),
                );
            });
        }
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let theme = self.theme;
        egui::TopBottomPanel::bottom("census_status")
            .frame(egui::Frame::NONE.fill(theme.panel()).inner_margin(egui::Margin::symmetric(12, 6)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let count = self.ids.len();
                    let text = if self.query.trim().is_empty() && !self.ai_mode {
                        format!("{} archivos y carpetas listos · escribi para buscar", self.index.len())
                    } else if self.truncated {
                        format!("{count} resultados (tope {CAP}) en {:.0} ms", self.elapsed_ms)
                    } else {
                        format!("{count} resultados en {:.0} ms", self.elapsed_ms)
                    };
                    ui.label(RichText::new(text).size(theme.font_status()).color(theme.text()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let nodes = self.index.len();
                        let ram = (nodes * std::mem::size_of::<crate::Node>() + self.index.names.len() * 2) as f64
                            / (1024.0 * 1024.0);
                        ui.label(
                            RichText::new(format!(
                                "{} {} · {nodes} nodos · {ram:.0} MB · Ctrl+C copia la ruta · Enter abre",
                                self.index.roots.len(),
                                if self.index.roots.len() == 1 { "unidad" } else { "unidades" }
                            ))
                            .size(theme.font_status())
                            .color(theme.muted()),
                        );
                    });
                });
            });
    }
}

fn human_size(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < U.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.fonts_ready {
            // Misma tipografía que WizEverything: Segoe UI + Segoe UI Symbol.
            Theme::setup_fonts(ctx);
            self.fonts_ready = true;
        }
        if self.applied_theme != Some(self.theme) {
            self.theme.apply(ctx);
            self.applied_theme = Some(self.theme);
        }
        self.drives_window(ctx);
        if ctx.input(|i| i.key_pressed(Key::Escape)) && !self.query.is_empty() {
            self.query.clear();
            self.applied_query.clear();
        }
        if ctx.input(|i| i.key_pressed(Key::Enter)) {
            if self.ai_mode {
                self.ask_ai(ctx);
            } else {
                self.open_selected();
            }
        }
        self.poll_ai(ctx);
        self.key_dialog(ctx);

        // Buscar sólo cuando cambió algo, y no con la consulta vacía.
        if self.query != self.applied_query {
            self.applied_query = self.query.clone();
            if self.query.trim().is_empty() {
                self.ids.clear();
                self.truncated = false;
                self.elapsed_ms = 0.0;
                self.selected = None;
            } else {
                self.refresh();
            }
        }

        self.top_bar(ctx);
        self.status_bar(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(self.theme.bg()).inner_margin(egui::Margin::symmetric(12, 8)))
            .show(ctx, |ui| {
                if self.ai_mode {
                    self.ai_panel(ui);
                } else if self.query.trim().is_empty() {
                    welcome(ui, &self.index, self.theme);
                } else {
                    self.header(ui);
                    self.rows(ui);
                }
            });

        if let Some(left) = self.smoke {
            if left == 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                self.smoke = Some(left - 1);
                ctx.request_repaint();
            }
        }
    }
}

fn welcome(ui: &mut egui::Ui, index: &Index, theme: Theme) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("Escribí para buscar").size(theme.font_title() + 3.0).color(theme.text()));
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "{} archivos y {} carpetas indexados en el volumen {}",
                index.stats.files, index.stats.dirs, index.letter
            ))
            .size(12.5)
            .color(theme.muted()),
        );
        ui.add_space(18.0);
        for (q, what) in [
            ("path:\"C:\\Users\" file: ext:pdf", "Los PDF de una carpeta"),
            ("file: dm:today", "Lo modificado hoy"),
            ("file: size:>100mb", "Archivos de más de 100 MB"),
            ("rc:>=2026-01-01T00:00:00", "Lo cambiado en el último año"),
        ] {
            ui.label(RichText::new(q).monospace().size(12.0).color(theme.accent()));
            ui.label(RichText::new(what).size(theme.font_small()).color(theme.muted()));
            ui.add_space(8.0);
        }
    });
}

/// Abre la ventana. Con `smoke` abre y se cierra solo (chequeo de arranque).
pub fn run(index: Arc<Index>, opts: GuiOptions) -> Result<(), String> {
    let title = format!("Census — volumen {}", index.letter);
    // Menos 80 px para el marco: la ventana tiene que entrar completa.
    let (work_w, work_h) = crate::win::work_area();
    let w = (work_w as f32 - 40.0).clamp(720.0, 1180.0);
    let h = (work_h as f32 - 80.0).clamp(380.0, 760.0);
    eprintln!("ventana {w:.0}x{h:.0} (area de trabajo {work_w}x{work_h})");
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([w, h])
            .with_min_inner_size([720.0, 380.0])
            .with_title(title),
        ..Default::default()
    };
    eframe::run_native(
        "Census",
        native,
        Box::new(move |cc| {
            cc.egui_ctx.set_zoom_factor(1.0);
            Ok(Box::new(GuiApp::new(index, opts)))
        }),
    )
    .map_err(|e| format!("no se pudo abrir la ventana: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(4096), "4.0 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn columns_do_not_overlap() {
        let app_like = GuiApp {
            index: Arc::new(Index::default()),
            query: String::new(),
            applied_query: String::new(),
            sort: Sort::NameAsc,
            theme: Theme::Dark,
            applied_theme: None,
            ids: Vec::new(),
            truncated: false,
            elapsed_ms: 0.0,
            selected: None,
            focus_search: false,
            smoke: None,
            fonts_ready: false,
            show_drives: false,
            excluded: Vec::new(),
            drives_dirty: false,
            ai_mode: false,
            ai_asking: false,
            ai_rx: None,
            ai_answer: None,
            ai_model: None,
            ai_error: None,
            ai_question: String::new(),
            key_dialog: false,
            key_input: String::new(),
        };
        let row = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(1000.0, Theme::Dark.row_h()));
        let name = app_like.column_rect(row, Column::Name);
        let path = app_like.column_rect(row, Column::Path);
        let size = app_like.column_rect(row, Column::Size);
        let date = app_like.column_rect(row, Column::Modified);
        assert!(name.right() <= path.left());
        assert!(path.right() <= size.left());
        assert!(size.right() <= date.left());
        assert!(date.right() <= row.right());
    }
}
