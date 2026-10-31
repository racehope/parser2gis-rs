//! Оконный интерфейс (egui).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Layout,
    Margin, RichText, Sense, Stroke, TextStyle, Vec2,
};
use egui_extras::{Column, TableBuilder};
use serde::{Deserialize, Serialize};

use crate::browser::ChromeOptions;
use crate::cities::{self, City};
use crate::events::{Event, LeadRow, LogLevel, Reporter, Summary, UiLogLayer};
use crate::filter::{ContactKind, Filters, WithoutMode};
use crate::job::{JobConfig, run_job};
use crate::lang::LangFilter;
use crate::parser::{ParserOptions, Stats};
use crate::writer::{Encoding, Format, WriterOptions};

// ------------------------------------------------------------------ палитра

const BG: Color32 = Color32::from_rgb(0x0f, 0x10, 0x12);
const PANEL: Color32 = Color32::from_rgb(0x15, 0x16, 0x19);
const CARD: Color32 = Color32::from_rgb(0x1b, 0x1c, 0x20);
const CARD_HI: Color32 = Color32::from_rgb(0x22, 0x23, 0x28);
const INPUT: Color32 = Color32::from_rgb(0x0d, 0x0e, 0x10);
const BORDER: Color32 = Color32::from_rgb(0x2a, 0x2b, 0x31);
const TEXT: Color32 = Color32::from_rgb(0xe9, 0xe6, 0xe1);
const MUTED: Color32 = Color32::from_rgb(0x8d, 0x8b, 0x87);
const GOLD: Color32 = Color32::from_rgb(0xcf, 0xa9, 0x5e);
const GOLD_HI: Color32 = Color32::from_rgb(0xe0, 0xbc, 0x72);
const GREEN: Color32 = Color32::from_rgb(0x6c, 0xc0, 0x8f);
const RED: Color32 = Color32::from_rgb(0xe2, 0x6d, 0x6d);
const AMBER: Color32 = Color32::from_rgb(0xe0, 0xa6, 0x4f);

// ------------------------------------------------------------------ настройки

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Source {
    City,
    Links,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum OutFormat {
    Xlsx,
    Csv,
    Json,
}

impl OutFormat {
    fn ext(self) -> &'static str {
        match self {
            Self::Xlsx => "xlsx",
            Self::Csv => "csv",
            Self::Json => "json",
        }
    }
    fn to_writer(self) -> Format {
        match self {
            Self::Xlsx => Format::Xlsx,
            Self::Csv => Format::Csv,
            Self::Json => Format::Json,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    source: Source,
    city_code: String,
    city_domain: String,
    queries: String,
    links: String,

    no_website: bool,
    no_instagram: bool,
    no_whatsapp: bool,
    mode_any: bool,
    lang: LangFilter,
    one_per_org: bool,
    need_phone: bool,
    use_min_reviews: bool,
    min_reviews: i64,
    use_max_reviews: bool,
    max_reviews: i64,
    use_min_rating: bool,
    min_rating: f64,
    strict_website: bool,

    format: OutFormat,
    output_dir: String,

    show_browser: bool,
    delay_ms: u64,
    max_records: usize,
    browser_path: String,
}

impl Default for Settings {
    fn default() -> Self {
        let out = dirs::document_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("2GIS Лиды");
        Self {
            source: Source::City,
            city_code: "kostanay".into(),
            city_domain: "kz".into(),
            queries: String::new(),
            links: String::new(),
            no_website: true,
            no_instagram: false,
            no_whatsapp: false,
            mode_any: false,
            lang: LangFilter::Any,
            one_per_org: true,
            need_phone: true,
            use_min_reviews: false,
            min_reviews: 10,
            use_max_reviews: false,
            max_reviews: 500,
            use_min_rating: false,
            min_rating: 4.0,
            strict_website: false,
            format: OutFormat::Xlsx,
            output_dir: out.to_string_lossy().into_owned(),
            show_browser: false,
            delay_ms: 0,
            max_records: 0,
            browser_path: String::new(),
        }
    }
}

// ------------------------------------------------------------------ состояние

#[derive(PartialEq)]
enum Tab {
    Leads,
    Log,
}

struct RunningJob {
    handle: Option<std::thread::JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
    started: Instant,
}

pub struct App {
    s: Settings,
    rx: Receiver<Event>,
    reporter: Reporter,
    job: Option<RunningJob>,
    leads: Vec<LeadRow>,
    logs: VecDeque<(LogLevel, String)>,
    stats: Stats,
    status: String,
    result: Option<Result<Summary, String>>,
    elapsed: Duration,
    form_error: Option<String>,
    tab: Tab,
    city_search: String,
    toast: Option<(String, Instant)>,
}

pub fn run() -> eframe::Result {
    let (tx, rx) = channel();
    let ctx_slot: Arc<std::sync::Mutex<Option<egui::Context>>> = Arc::default();
    let waker_slot = ctx_slot.clone();
    let reporter = Reporter::new(
        tx,
        Arc::new(move || {
            if let Some(ctx) = waker_slot.lock().unwrap().as_ref() {
                ctx.request_repaint();
            }
        }),
    );

    {
        use tracing_subscriber::prelude::*;
        let filter = tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,chromiumoxide=off,tungstenite=off".into()),
        );
        tracing_subscriber::registry()
            .with(filter)
            .with(UiLogLayer { reporter: reporter.clone() })
            .init();
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("2GIS Лиды")
            .with_inner_size([1320.0, 820.0])
            .with_min_inner_size([1000.0, 640.0])
            .with_icon(app_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "parser-2gis",
        options,
        Box::new(move |cc| {
            *ctx_slot.lock().unwrap() = Some(cc.egui_ctx.clone());
            setup_style(&cc.egui_ctx);
            let s: Settings = cc
                .storage
                .and_then(|st| eframe::get_value(st, eframe::APP_KEY))
                .unwrap_or_default();
            Ok(Box::new(App {
                s,
                rx,
                reporter,
                job: None,
                leads: Vec::new(),
                logs: VecDeque::new(),
                stats: Stats::default(),
                status: String::new(),
                result: None,
                elapsed: Duration::ZERO,
                form_error: None,
                tab: Tab::Leads,
                city_search: String::new(),
                toast: None,
            }))
        }),
    )
}

// ------------------------------------------------------------------ стиль

fn setup_style(ctx: &egui::Context) {
    // Шрифты: Segoe UI в Windows, иначе встроенные egui (с кириллицей)
    let mut fonts = FontDefinitions::default();
    let candidates: &[(&str, &[&str])] = &[
        (
            "ui",
            &[
                r"C:\Windows\Fonts\segoeui.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            ],
        ),
        (
            "ui-bold",
            &[
                r"C:\Windows\Fonts\seguisb.ttf",
                r"C:\Windows\Fonts\segoeuib.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            ],
        ),
    ];
    let mut bold_loaded = false;
    for (name, paths) in candidates {
        if let Some(bytes) = paths.iter().find_map(|p| std::fs::read(p).ok()) {
            fonts.font_data.insert((*name).into(), Arc::new(FontData::from_owned(bytes)));
            if *name == "ui" {
                fonts
                    .families
                    .entry(FontFamily::Proportional)
                    .or_default()
                    .insert(0, "ui".into());
            } else {
                bold_loaded = true;
            }
        }
    }
    let mut bold_family = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if bold_loaded {
        bold_family.insert(0, "ui-bold".into());
    }
    fonts.families.insert(FontFamily::Name("bold".into()), bold_family);
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Body, FontId::proportional(14.5)),
            (TextStyle::Button, FontId::proportional(14.5)),
            (TextStyle::Heading, FontId::new(20.0, FontFamily::Name("bold".into()))),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.combo_height = 360.0;

        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = PANEL;
        v.window_fill = CARD;
        v.extreme_bg_color = INPUT;
        v.faint_bg_color = Color32::from_rgb(0x19, 0x1a, 0x1e);
        v.code_bg_color = INPUT;
        v.hyperlink_color = GOLD_HI;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(10);
        v.selection.bg_fill = GOLD.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, GOLD_HI);
        v.override_text_color = None;

        let r = CornerRadius::same(8);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CARD;
        w.noninteractive.weak_bg_fill = CARD;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = r;
        for (st, fill) in [
            (&mut w.inactive, Color32::from_rgb(0x24, 0x25, 0x2b)),
            (&mut w.hovered, Color32::from_rgb(0x2e, 0x2f, 0x36)),
            (&mut w.active, Color32::from_rgb(0x37, 0x38, 0x40)),
            (&mut w.open, Color32::from_rgb(0x2e, 0x2f, 0x36)),
        ] {
            st.bg_fill = fill;
            st.weak_bg_fill = fill;
            st.corner_radius = r;
            st.fg_stroke = Stroke::new(1.0, TEXT);
            st.bg_stroke = Stroke::new(1.0, BORDER);
        }
        w.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x3b, 0x3c, 0x44));
        w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x55, 0x56, 0x5e));
        w.active.bg_stroke = Stroke::new(1.0, GOLD);
    });
}

fn app_icon() -> egui::IconData {
    // Золотой ромб на тёмном скруглённом квадрате
    let n = 64usize;
    let mut rgba = vec![0u8; n * n * 4];
    let c = (n as f32 - 1.0) / 2.0;
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 - c, y as f32 - c);
            let i = (y * n + x) * 4;
            let rr = 14.0;
            let qx = (fx.abs() - (c - rr)).max(0.0);
            let qy = (fy.abs() - (c - rr)).max(0.0);
            let inside_bg = (qx * qx + qy * qy).sqrt() <= rr;
            if !inside_bg {
                continue;
            }
            let d = fx.abs() + fy.abs();
            let (r, g, b) = if d < 20.0 {
                (0xcf, 0xa9, 0x5e)
            } else if d < 23.0 {
                (0x6b, 0x58, 0x33)
            } else {
                (0x16, 0x17, 0x1a)
            };
            rgba[i..i + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }
    egui::IconData { rgba, width: n as u32, height: n as u32 }
}

fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(12)
        .inner_margin(Margin::same(14))
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(
        RichText::new(title.to_uppercase())
            .size(11.5)
            .color(MUTED)
            .family(FontFamily::Name("bold".into())),
    );
    ui.add_space(2.0);
}

fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = Vec2::new(40.0, 22.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(resp.id, *on);
        let off_bg = Color32::from_rgb(0x30, 0x31, 0x38);
        let bg = lerp_color(off_bg, GOLD, t);
        let radius = rect.height() / 2.0;
        ui.painter().rect_filled(rect, radius, bg);
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        let knob = lerp_color(Color32::from_rgb(0xb8, 0xb6, 0xb2), Color32::from_rgb(0x1a, 0x16, 0x0e), t);
        ui.painter().circle_filled(egui::pos2(x, rect.center().y), radius - 4.0, knob);
    }
    resp
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Строка «подпись ... переключатель».
fn toggle_row(ui: &mut egui::Ui, label: &str, hint: Option<&str>, on: &mut bool) {
    ui.horizontal(|ui| {
        let text_w = ui.available_width() - 52.0;
        ui.vertical(|ui| {
            ui.set_max_width(text_w);
            ui.spacing_mut().item_spacing.y = 1.0;
            let r = ui.add(egui::Label::new(RichText::new(label).color(TEXT)).sense(Sense::click()));
            if r.clicked() {
                *on = !*on;
            }
            if let Some(h) = hint {
                ui.add(egui::Label::new(RichText::new(h).small().color(MUTED)).wrap());
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            toggle(ui, on);
        });
    });
}

fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) {
    egui::Frame::new()
        .fill(INPUT)
        .corner_radius(9)
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.spacing_mut().button_padding = Vec2::new(4.0, 4.0);
            let w = ((ui.available_width() - 3.0 * (options.len() as f32 - 1.0)) / options.len() as f32).floor();
            let size = if options.len() >= 3 { 13.5 } else { 14.5 };
            ui.horizontal(|ui| {
                for (v, label) in options {
                    let selected = *value == *v;
                    let text = RichText::new(*label).size(size).color(if selected { Color32::from_rgb(0x1a, 0x16, 0x0e) } else { MUTED });
                    let btn = egui::Button::new(text)
                        .fill(if selected { GOLD } else { Color32::TRANSPARENT })
                        .stroke(Stroke::NONE)
                        .corner_radius(7)
                        .min_size(Vec2::new(w, 28.0));
                    if ui.add(btn).clicked() {
                        *value = *v;
                    }
                }
            });
        });
}

fn primary_button(text: &str, fill: Color32, fg: Color32) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text.to_string())
            .size(15.5)
            .color(fg)
            .family(FontFamily::Name("bold".into())),
    )
    .fill(fill)
    .stroke(Stroke::NONE)
    .corner_radius(10)
    .min_size(Vec2::new(0.0, 44.0))
}

fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn file_slug(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    out = out.chars().take(40).collect();
    if out.is_empty() { "2gis".into() } else { out }
}

fn open_path(p: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(p).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(p).spawn();
    }
}

fn reveal_path(p: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", p.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        if let Some(dir) = p.parent() {
            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
        }
    }
}

fn with_scheme(u: &str) -> String {
    if u.starts_with("http://") || u.starts_with("https://") {
        u.to_string()
    } else {
        format!("https://{u}")
    }
}

// ------------------------------------------------------------------ логика

impl App {
    fn running(&self) -> bool {
        self.job.is_some()
    }

    /// "*" в city_code = вся страна (city_domain тогда хранит код страны)
    fn whole_country(&self) -> bool {
        self.s.city_code == "*"
    }

    fn selected_cities(&self) -> Vec<&'static City> {
        if self.whole_country() {
            cities::all().iter().filter(|c| c.country_code == self.s.city_domain).collect()
        } else {
            cities::find(&self.s.city_code, &self.s.city_domain).into_iter().collect()
        }
    }

    fn selection_title(&self) -> Option<String> {
        if self.whole_country() {
            let n = self.selected_cities().len();
            (n > 0).then(|| {
                format!("Вся страна: {}  ·  {n} {}", cities::country_name(&self.s.city_domain), plural(n, "город", "города", "городов"))
            })
        } else {
            self.selected_cities()
                .first()
                .map(|c| format!("{}  ·  {}", c.name, cities::country_name(&c.country_code)))
        }
    }

    fn queries(&self) -> Vec<String> {
        self.s
            .queries
            .split([',', '\n', ';'])
            .map(|q| q.trim().to_string())
            .filter(|q| !q.is_empty())
            .collect()
    }

    fn build_config(&self) -> Result<JobConfig, String> {
        let (urls, labels, slug) = match self.s.source {
            Source::City => {
                let cities = self.selected_cities();
                if cities.is_empty() {
                    return Err("Выберите город".into());
                }
                let qs = self.queries();
                if qs.is_empty() {
                    return Err("Введите, что искать — например «барбершоп»".into());
                }
                let mut urls = Vec::new();
                let mut labels = Vec::new();
                for c in &cities {
                    for q in &qs {
                        urls.push(c.search_url(q));
                        labels.push(format!("{} · {q}", c.name));
                    }
                }
                let place = if self.whole_country() {
                    cities::country_name(&self.s.city_domain).to_string()
                } else {
                    cities[0].name.clone()
                };
                (urls, labels, format!("{}_{place}", qs[0]))
            }
            Source::Links => {
                let urls: Vec<String> = self
                    .s
                    .links
                    .lines()
                    .map(str::trim)
                    .filter(|l| l.starts_with("http"))
                    .map(String::from)
                    .collect();
                if urls.is_empty() {
                    return Err("Вставьте хотя бы одну ссылку 2GIS".into());
                }
                (urls, Vec::new(), "ссылки".to_string())
            }
        };

        if self.s.output_dir.trim().is_empty() {
            return Err("Укажите папку для сохранения".into());
        }
        let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M");
        let output = PathBuf::from(self.s.output_dir.trim())
            .join(format!("{}_{stamp}.{}", file_slug(&slug), self.s.format.ext()));

        let mut without = Vec::new();
        if self.s.no_website {
            without.push(ContactKind::Website);
        }
        if self.s.no_instagram {
            without.push(ContactKind::Instagram);
        }
        if self.s.no_whatsapp {
            without.push(ContactKind::Whatsapp);
        }
        let filters = Filters {
            without,
            without_mode: if self.s.mode_any { WithoutMode::Any } else { WithoutMode::All },
            with: if self.s.need_phone { vec![ContactKind::Phone] } else { vec![] },
            min_reviews: self.s.use_min_reviews.then_some(self.s.min_reviews),
            max_reviews: self.s.use_max_reviews.then_some(self.s.max_reviews),
            min_rating: self.s.use_min_rating.then_some(self.s.min_rating),
            max_rating: None,
            strict_website: self.s.strict_website,
            include_non_firms: false,
            lang: self.s.lang,
            one_per_org: self.s.one_per_org,
        };

        let browser_path = self.s.browser_path.trim();
        Ok(JobConfig {
            urls,
            labels,
            output,
            format: self.s.format.to_writer(),
            filters,
            chrome: ChromeOptions {
                binary_path: (!browser_path.is_empty()).then(|| PathBuf::from(browser_path)),
                headless: !self.s.show_browser,
                disable_images: true,
                start_maximized: false,
                memory_limit: 4096,
                extra_args: std::env::var("P2G_CHROME_ARGS")
                    .map(|a| a.split('|').map(String::from).collect())
                    .unwrap_or_default(),
            },
            parser: ParserOptions {
                skip_404_response: true,
                delay_between_clicks: self.s.delay_ms,
                max_records: if self.s.max_records == 0 { usize::MAX } else { self.s.max_records },
                gc_pages_interval: 10,
                verbose: true,
            },
            writer: WriterOptions {
                encoding: Encoding::Utf8Sig,
                add_rubrics: true,
                add_comments: true,
                columns_per_entity: 3,
                remove_empty_columns: true,
                remove_duplicates: true,
                join_char: "; ".into(),
            },
        })
    }

    fn start(&mut self) {
        let cfg = match self.build_config() {
            Ok(c) => c,
            Err(e) => {
                self.form_error = Some(e);
                return;
            }
        };
        self.form_error = None;
        self.leads.clear();
        self.logs.clear();
        self.stats = Stats::default();
        self.result = None;
        self.status = "запуск".into();
        self.tab = Tab::Leads;

        let cancel = Arc::new(AtomicBool::new(false));
        let reporter = self.reporter.clone();
        let c2 = cancel.clone();
        let handle = std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    reporter.send(Event::Done(Err(format!("не удалось запустить рантайм: {e}"))));
                    return;
                }
            };
            let res = rt.block_on(run_job(cfg, reporter.clone(), c2));
            rt.shutdown_timeout(Duration::from_secs(2));
            reporter.send(Event::Done(res.map_err(|e| format!("{e:#}"))));
        });
        self.job = Some(RunningJob { handle: Some(handle), cancel, started: Instant::now() });
    }

    fn stop(&mut self) {
        if let Some(j) = &self.job {
            j.cancel.store(true, Ordering::Relaxed);
            self.status = "остановка — сохраняю собранное…".into();
        }
    }

    fn pump_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Lead(l) => self.leads.push(l),
                Event::Stats(s) => self.stats = s,
                Event::Status(s) => self.status = s,
                Event::Log(level, msg) => {
                    if level != LogLevel::Debug && !msg.starts_with("Парсинг [") {
                        self.logs.push_back((level, msg));
                        while self.logs.len() > 3000 {
                            self.logs.pop_front();
                        }
                    }
                }
                Event::Done(res) => {
                    if let Some(mut j) = self.job.take() {
                        self.elapsed = j.started.elapsed();
                        if let Some(h) = j.handle.take() {
                            let _ = h.join();
                        }
                    }
                    if let Ok(s) = &res {
                        self.stats = s.stats;
                    }
                    self.result = Some(res);
                    self.status.clear();
                }
            }
        }
    }

    // -------------------------------------------------------------- отрисовка

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
            let c = rect.center();
            let r = 11.0;
            ui.painter().add(egui::Shape::convex_polygon(
                vec![c + Vec2::new(0.0, -r), c + Vec2::new(r, 0.0), c + Vec2::new(0.0, r), c + Vec2::new(-r, 0.0)],
                GOLD,
                Stroke::NONE,
            ));
            ui.label(RichText::new("2GIS Лиды").size(19.0).color(TEXT).family(FontFamily::Name("bold".into())));
            ui.label(RichText::new("поиск организаций без сайта и соцсетей").color(MUTED));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (text, color) = if self.running() {
                    ("Идёт парсинг", GOLD)
                } else {
                    match &self.result {
                        None => ("Готов к запуску", MUTED),
                        Some(Ok(s)) if s.cancelled => ("Остановлено", AMBER),
                        Some(Ok(_)) => ("Готово", GREEN),
                        Some(Err(_)) => ("Ошибка", RED),
                    }
                };
                egui::Frame::new()
                    .fill(color.gamma_multiply(0.14))
                    .stroke(Stroke::new(1.0, color.gamma_multiply(0.5)))
                    .corner_radius(20)
                    .inner_margin(Margin::symmetric(12, 5))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                            ui.painter().circle_filled(r.center(), 4.0, color);
                            ui.label(RichText::new(text).color(color).size(13.0));
                        });
                    });
            });
        });
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui) {
        let running = self.running();
        ui.add_enabled_ui(!running, |ui| {
            // ---------------------------------------------- где искать
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                section_title(ui, "Где искать");
                segmented(ui, &mut self.s.source, &[(Source::City, "Город и запрос"), (Source::Links, "Свои ссылки")]);
                ui.add_space(4.0);
                match self.s.source {
                    Source::City => self.city_picker(ui),
                    Source::Links => {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.s.links)
                                .hint_text("https://2gis.kz/almaty/search/кофейня\nпо одной ссылке на строку")
                                .desired_rows(5)
                                .desired_width(f32::INFINITY),
                        );
                        ui.label(RichText::new("Поиск, рубрика, карточка фирмы или «в здании»").small().color(MUTED));
                    }
                }
            });

            ui.add_space(10.0);

            // ---------------------------------------------- фильтры
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                section_title(ui, "Фильтры");
                toggle_row(ui, "Без сайта", Some("Instagram и taplink в поле «Сайт» не считаются"), &mut self.s.no_website);
                toggle_row(ui, "Без Instagram", None, &mut self.s.no_instagram);
                toggle_row(ui, "Без WhatsApp", None, &mut self.s.no_whatsapp);

                let n = [self.s.no_website, self.s.no_instagram, self.s.no_whatsapp].iter().filter(|x| **x).count();
                if n >= 2 {
                    ui.add_space(2.0);
                    let mut any = self.s.mode_any;
                    segmented(ui, &mut any, &[(false, "Нет ни одного"), (true, "Нет хотя бы одного")]);
                    self.s.mode_any = any;
                }

                ui.separator();
                toggle_row(ui, "Только с телефоном", None, &mut self.s.need_phone);
                toggle_row(ui, "Один филиал от сети", Some("Остальные филиалы той же организации пропускаются"), &mut self.s.one_per_org);
                ui.separator();
                ui.label(RichText::new("Язык названия").color(TEXT));
                segmented(ui, &mut self.s.lang, &[(LangFilter::Any, "Любой"), (LangFilter::NotKazakh, "Без казахских"), (LangFilter::Russian, "Только русские")]);
                ui.add(
                    egui::Label::new(
                        RichText::new(match self.s.lang {
                            LangFilter::Any => "Определяется примерно — по названию организации",
                            LangFilter::NotKazakh => "Отсеивает названия с казахскими буквами и словами. Латиница (Barber King) остаётся",
                            LangFilter::Russian => "Только названия на русском. Латиница (Barber King) тоже отсеивается",
                            LangFilter::Kazakh => "Только казахские названия",
                        })
                        .small()
                        .color(MUTED),
                    )
                    .wrap(),
                );
                ui.separator();

                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.s.use_min_reviews, "Отзывов от");
                    ui.add_enabled(self.s.use_min_reviews, egui::DragValue::new(&mut self.s.min_reviews).range(0..=100_000));
                    ui.checkbox(&mut self.s.use_max_reviews, "до");
                    ui.add_enabled(self.s.use_max_reviews, egui::DragValue::new(&mut self.s.max_reviews).range(0..=100_000));
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.s.use_min_rating, "Рейтинг от");
                    ui.add_enabled(
                        self.s.use_min_rating,
                        egui::Slider::new(&mut self.s.min_rating, 1.0..=5.0).step_by(0.1).fixed_decimals(1),
                    );
                });
            });

            ui.add_space(10.0);

            // ---------------------------------------------- сохранение
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                section_title(ui, "Сохранение");
                segmented(ui, &mut self.s.format, &[(OutFormat::Xlsx, "Excel"), (OutFormat::Csv, "CSV"), (OutFormat::Json, "JSON")]);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Папка").color(MUTED));
                    let short = shorten_path(&self.s.output_dir, 34);
                    ui.label(RichText::new(short).color(TEXT)).on_hover_text(&self.s.output_dir);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Изменить…").clicked() {
                            if let Some(dir) = rfd::FileDialog::new().set_directory(&self.s.output_dir).pick_folder() {
                                self.s.output_dir = dir.to_string_lossy().into_owned();
                            }
                        }
                    });
                });
            });

            ui.add_space(10.0);

            // ---------------------------------------------- дополнительно
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::CollapsingHeader::new(RichText::new("ДОПОЛНИТЕЛЬНО").size(11.5).color(MUTED).family(FontFamily::Name("bold".into())))
                    .default_open(false)
                    .show(ui, |ui| {
                        toggle_row(ui, "Показывать окно браузера", Some("Включите, если 2GIS не отдаёт данные"), &mut self.s.show_browser);
                        toggle_row(ui, "Считать соцсети в поле «Сайт» сайтом", None, &mut self.s.strict_website);
                        egui::Grid::new("adv").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                            ui.label("Пауза между кликами, мс");
                            ui.add(egui::DragValue::new(&mut self.s.delay_ms).range(0..=10_000).speed(10));
                            ui.end_row();
                            ui.label("Лимит с одной ссылки");
                            ui.add(egui::DragValue::new(&mut self.s.max_records).range(0..=100_000).custom_formatter(|v, _| {
                                if v == 0.0 { "без лимита".into() } else { format!("{v}") }
                            }));
                            ui.end_row();
                        });
                        ui.label(RichText::new("Браузер (пусто — найти автоматически)").small().color(MUTED));
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.s.browser_path)
                                    .hint_text("Chrome / Edge / Яндекс.Браузер")
                                    .desired_width(ui.available_width() - 80.0),
                            );
                            if ui.button("Обзор…").clicked() {
                                if let Some(f) = rfd::FileDialog::new().add_filter("Программа", &["exe"]).pick_file() {
                                    self.s.browser_path = f.to_string_lossy().into_owned();
                                }
                            }
                        });
                    });
            });
        });
    }

    fn city_picker(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Город").color(MUTED).small());
        let selected = self.selection_title().unwrap_or_else(|| "Выберите город".into());
        let width = ui.available_width();
        egui::ComboBox::from_id_salt("city")
            .selected_text(selected)
            .width(width)
            .height(380.0)
            .show_ui(ui, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.city_search)
                        .hint_text("Поиск города…")
                        .desired_width(f32::INFINITY),
                )
                .request_focus();
                let q = self.city_search.to_lowercase();
                let mut last_cc = "";
                for c in cities::all() {
                    if !q.is_empty() && !c.name.to_lowercase().contains(&q) {
                        continue;
                    }
                    if c.country_code != last_cc {
                        last_cc = &c.country_code;
                        ui.add_space(4.0);
                        ui.label(RichText::new(cities::country_name(last_cc)).small().color(GOLD));
                        if q.is_empty() {
                            let n = cities::all().iter().filter(|x| x.country_code == last_cc).count();
                            if n > 1 {
                                let is_sel = self.whole_country() && self.s.city_domain == last_cc;
                                let text = RichText::new(format!("Вся страна — {n} {}", plural(n, "город", "города", "городов"))).color(GOLD_HI);
                                if ui.selectable_label(is_sel, text).clicked() {
                                    self.s.city_code = "*".into();
                                    self.s.city_domain = last_cc.to_string();
                                    ui.close();
                                }
                            }
                        }
                    }
                    let is_sel = c.code == self.s.city_code && c.domain == self.s.city_domain;
                    if ui.selectable_label(is_sel, &c.name).clicked() {
                        self.s.city_code = c.code.clone();
                        self.s.city_domain = c.domain.clone();
                        self.city_search.clear();
                        ui.close();
                    }
                }
            });
        ui.add_space(2.0);
        ui.label(RichText::new("Что ищем").color(MUTED).small());
        ui.add(
            egui::TextEdit::singleline(&mut self.s.queries)
                .hint_text("барбершоп, салон красоты, кофейня")
                .desired_width(f32::INFINITY),
        );
        let n = self.queries().len();
        let cities = self.selected_cities().len();
        let total = n * cities;
        if total > 1 {
            let mut t = format!("{total} {} поиска — всё в один файл", plural(total, "вариант", "варианта", "вариантов"));
            if total > 30 {
                t.push_str(". Это надолго — можно остановить в любой момент, собранное сохранится");
            }
            ui.add(egui::Label::new(RichText::new(t).small().color(MUTED)).wrap());
        }
    }

    fn start_area(&mut self, ui: &mut egui::Ui) {
        if let Some(err) = &self.form_error {
            ui.label(RichText::new(err).color(RED));
        }
        let w = ui.available_width();
        if self.running() {
            let stopping = self.job.as_ref().is_some_and(|j| j.cancel.load(Ordering::Relaxed));
            let btn = primary_button(if stopping { "Останавливаю…" } else { "Остановить" }, Color32::from_rgb(0x3a, 0x22, 0x22), RED);
            if ui.add_enabled_ui(!stopping, |ui| ui.add_sized([w, 44.0], btn)).inner.clicked() {
                self.stop();
            }
        } else {
            let btn = primary_button("Начать поиск", GOLD, Color32::from_rgb(0x1a, 0x16, 0x0e));
            if ui.add_sized([w, 44.0], btn).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                self.start();
            }
        }
    }

    fn stat_card(ui: &mut egui::Ui, width: f32, label: &str, value: String, accent: Color32) {
        card().show(ui, |ui| {
            ui.set_width(width - 28.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new(label).small().color(MUTED));
                ui.label(RichText::new(value).size(28.0).color(accent).family(FontFamily::Name("bold".into())));
            });
        });
    }

    fn results_panel(&mut self, ui: &mut egui::Ui) {
        // ---------------------------------------------- счётчики
        let gap = 10.0;
        let w = (ui.available_width() - gap * 4.0) / 5.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            Self::stat_card(ui, w, "Подходит", self.stats.written.to_string(), GOLD_HI);
            Self::stat_card(ui, w, "Просмотрено", self.stats.scanned.to_string(), TEXT);
            Self::stat_card(ui, w, "Отсеяно фильтрами", self.stats.filtered.to_string(), TEXT);
            Self::stat_card(ui, w, "Повторы", self.stats.duplicates.to_string(), TEXT);
            Self::stat_card(ui, w, "Ошибки", self.stats.errors.to_string(), if self.stats.errors > 0 { AMBER } else { TEXT });
        });

        ui.add_space(8.0);

        // ---------------------------------------------- статус / итог
        if let Some(job) = &self.job {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().color(GOLD).size(16.0));
                ui.label(RichText::new(if self.status.is_empty() { "работаю…" } else { &self.status }).color(TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(fmt_elapsed(job.started.elapsed())).color(MUTED).monospace());
                });
            });
        } else if let Some(res) = &self.result {
            let (color, title, body, path) = match res {
                Ok(s) => (
                    if s.cancelled { AMBER } else { GREEN },
                    if s.cancelled { "Остановлено — собранное сохранено" } else { "Готово" },
                    format!(
                        "В файле {} {} · заняло {}",
                        s.saved,
                        plural(s.saved, "запись", "записи", "записей"),
                        fmt_elapsed(self.elapsed)
                    ),
                    Some(s.path.clone()),
                ),
                Err(e) => (RED, "Ошибка", e.clone(), None),
            };
            egui::Frame::new()
                .fill(color.gamma_multiply(0.10))
                .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
                .corner_radius(12)
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(RichText::new(title).color(color).family(FontFamily::Name("bold".into())));
                            ui.label(RichText::new(body).color(TEXT));
                        });
                        if let Some(p) = path {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.button("Показать в папке").clicked() {
                                    reveal_path(&p);
                                }
                                if ui.add(egui::Button::new(RichText::new("Открыть файл").color(Color32::from_rgb(0x1a, 0x16, 0x0e))).fill(GOLD)).clicked() {
                                    open_path(&p);
                                }
                            });
                        }
                    });
                });
        } else {
            ui.label(RichText::new("Выберите город, запрос и фильтры слева — и жмите «Начать поиск».").color(MUTED));
        }

        ui.add_space(6.0);

        // ---------------------------------------------- вкладки
        ui.horizontal(|ui| {
            let leads_label = format!("Найдено  {}", self.leads.len());
            for (tab, label) in [(Tab::Leads, leads_label.as_str()), (Tab::Log, "Журнал")] {
                let sel = self.tab == tab;
                let r = ui.add(
                    egui::Button::new(RichText::new(label).color(if sel { TEXT } else { MUTED }))
                        .fill(if sel { CARD_HI } else { Color32::TRANSPARENT })
                        .stroke(if sel { Stroke::new(1.0, BORDER) } else { Stroke::NONE })
                        .corner_radius(8),
                );
                if r.clicked() {
                    self.tab = tab;
                }
            }
        });

        card().inner_margin(Margin::same(8)).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            match self.tab {
                Tab::Leads => self.leads_table(ui),
                Tab::Log => self.log_view(ui),
            }
        });
    }

    fn leads_table(&mut self, ui: &mut egui::Ui) {
        if self.leads.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new(if self.running() {
                        "Ищу подходящие организации…"
                    } else {
                        "Здесь появятся организации, прошедшие фильтры"
                    })
                    .color(MUTED),
                );
            });
            return;
        }

        let ctx = ui.ctx().clone();
        let mut toast = None;
        let running = self.running();
        let leads = &self.leads;
        egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .stick_to_bottom(running)
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::exact(34.0))
                .column(Column::initial(200.0).at_least(120.0).clip(true))
                .column(Column::initial(118.0).at_least(90.0).clip(true))
                .column(Column::initial(60.0))
                .column(Column::initial(42.0))
                .column(Column::initial(40.0))
                .column(Column::initial(130.0).clip(true))
                .column(Column::initial(130.0).clip(true))
                .column(Column::initial(120.0).clip(true))
                .column(Column::remainder().at_least(180.0).clip(true))
                .header(26.0, |mut h| {
                    for t in ["#", "Название", "Телефон", "Отзывы", "★", "Яз.", "Сайт", "Instagram", "WhatsApp", "Адрес"] {
                        h.col(|ui| {
                            ui.label(RichText::new(t).small().color(MUTED).family(FontFamily::Name("bold".into())));
                        });
                    }
                })
                .body(|body| {
                    body.rows(28.0, leads.len(), |mut row| {
                        let i = row.index();
                        let l = &leads[i];
                        row.col(|ui| {
                            ui.label(RichText::new((i + 1).to_string()).color(MUTED).small());
                        });
                        row.col(|ui| {
                            let r = ui.add(egui::Label::new(RichText::new(&l.name).color(TEXT)).sense(Sense::click()).truncate());
                            let hover = if l.branches > 1 {
                                format!("Открыть в 2GIS · филиалов в сети: {}", l.branches)
                            } else {
                                "Открыть в 2GIS".to_string()
                            };
                            if r.on_hover_text(hover).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                ctx.open_url(egui::OpenUrl::new_tab(&l.url));
                            }
                        });
                        row.col(|ui| {
                            if !l.phone.is_empty() {
                                let r = ui.add(egui::Label::new(RichText::new(&l.phone).monospace()).sense(Sense::click()));
                                if r.on_hover_text("Скопировать").on_hover_cursor(egui::CursorIcon::Copy).clicked() {
                                    ctx.copy_text(l.phone.clone());
                                    toast = Some(format!("Скопировано: {}", l.phone));
                                }
                            }
                        });
                        row.col(|ui| {
                            ui.label(l.reviews.to_string());
                        });
                        row.col(|ui| {
                            if let Some(r) = l.rating {
                                ui.label(RichText::new(format!("{r:.1}")).color(GOLD_HI));
                            }
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(l.lang).small().color(MUTED));
                        });
                        for v in [&l.website, &l.instagram, &l.whatsapp] {
                            row.col(|ui| {
                                if v.is_empty() {
                                    ui.label(RichText::new("—").color(Color32::from_rgb(0x4a, 0x4a, 0x50)));
                                } else {
                                    let short = v.trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.");
                                    let r = ui.add(egui::Label::new(RichText::new(short).color(GOLD_HI)).sense(Sense::click()).truncate());
                                    if r.on_hover_text(v.as_str()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                        ctx.open_url(egui::OpenUrl::new_tab(with_scheme(v)));
                                    }
                                }
                            });
                        }
                        row.col(|ui| {
                            ui.add(egui::Label::new(RichText::new(&l.address).color(MUTED)).truncate());
                        });
                    });
                });
        });
        if let Some(t) = toast {
            self.toast = Some((t, Instant::now()));
        }
    }

    fn log_view(&mut self, ui: &mut egui::Ui) {
        if self.logs.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Журнал пуст").color(MUTED));
            });
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                for (level, msg) in &self.logs {
                    let skipped = msg.trim_start().starts_with("пропуск");
                    let color = match level {
                        LogLevel::Error => RED,
                        LogLevel::Warn => AMBER,
                        _ if skipped => MUTED,
                        _ => TEXT,
                    };
                    ui.label(RichText::new(msg.trim_start()).monospace().color(color));
                }
            });
    }
}

fn plural(n: usize, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    let (n10, n100) = (n % 10, n % 100);
    if n10 == 1 && n100 != 11 {
        one
    } else if (2..=4).contains(&n10) && !(12..=14).contains(&n100) {
        few
    } else {
        many
    }
}

fn shorten_path(p: &str, max: usize) -> String {
    let chars: Vec<char> = p.chars().collect();
    if chars.len() <= max {
        return p.to_string();
    }
    let tail: String = chars[chars.len() - (max - 1)..].iter().collect();
    format!("…{tail}")
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump_events();
        if self.running() {
            // Обновляем таймер, даже если событий нет
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }

        egui::Panel::top("top")
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(16, 12)))
            .show(ui, |ui| self.top_bar(ui));

        egui::Panel::left("settings")
            .exact_size(412.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin { left: 16, right: 8, top: 4, bottom: 16 }))
            .show(ui, |ui| {
                egui::Panel::bottom("start")
                    .frame(egui::Frame::new().fill(BG).inner_margin(Margin { left: 0, right: 0, top: 10, bottom: 0 }))
                    .show(ui, |ui| self.start_area(ui));
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_space(4.0);
                    self.settings_panel(ui);
                    ui.add_space(4.0);
                });
            });

        egui::CentralPanel::no_frame()
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin { left: 8, right: 16, top: 4, bottom: 16 }))
            .show(ui, |ui| self.results_panel(ui));

        // всплывающее уведомление
        if let Some((text, at)) = &self.toast {
            if at.elapsed() < Duration::from_millis(1800) {
                egui::Area::new(egui::Id::NULL.with("toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -24.0))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::new()
                            .fill(CARD_HI)
                            .stroke(Stroke::new(1.0, BORDER))
                            .corner_radius(10)
                            .inner_margin(Margin::symmetric(14, 8))
                            .show(ui, |ui| ui.label(RichText::new(text).color(TEXT)));
                    });
                ui.ctx().request_repaint_after(Duration::from_millis(200));
            } else {
                self.toast = None;
            }
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.s);
    }

    fn on_exit(&mut self) {
        // Окно закрыли посреди работы — останавливаем и даём дописать файл
        if let Some(mut j) = self.job.take() {
            j.cancel.store(true, Ordering::Relaxed);
            if let Some(h) = j.handle.take() {
                let deadline = Instant::now() + Duration::from_secs(10);
                while !h.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}
