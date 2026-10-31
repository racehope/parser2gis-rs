//! События парсера для интерфейса: найденные лиды, статистика, статус, журнал.

use std::sync::mpsc::Sender;
use std::sync::Arc;

use regex::Regex;
use tracing::field::{Field, Visit};
use tracing::{Event as TracingEvent, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

use crate::filter::{ContactKind, classify_website};
use crate::model::CatalogItem;
use crate::parser::Stats;

#[derive(Debug, Clone)]
pub struct LeadRow {
    pub name: String,
    pub address: String,
    pub phone: String,
    pub website: String,
    pub instagram: String,
    pub whatsapp: String,
    pub reviews: i64,
    pub rating: Option<f64>,
    pub url: String,
    pub lang: &'static str,
    pub branches: i64,
}

impl LeadRow {
    pub fn from_item(item: &CatalogItem) -> Self {
        let phone_re = Regex::new(r"[^0-9+]").unwrap();
        let mut row = LeadRow {
            name: item.display_name(),
            address: item.address_name.clone().unwrap_or_default(),
            phone: String::new(),
            website: String::new(),
            instagram: String::new(),
            whatsapp: String::new(),
            reviews: item.review_count(),
            rating: item.rating(),
            url: item.url(),
            lang: crate::lang::detect(&item.display_name()).short(),
            branches: item.branch_count(),
        };
        let first = |slot: &mut String, v: Option<&String>| {
            if slot.is_empty() {
                if let Some(v) = v.filter(|v| !v.is_empty()) {
                    *slot = v.clone();
                }
            }
        };
        for c in item.contacts() {
            match c.kind.as_str() {
                "phone" => {
                    if row.phone.is_empty() {
                        if let Some(p) = crate::writer::contact_value(c, &phone_re) {
                            row.phone = p;
                        }
                    }
                }
                "instagram" => first(&mut row.instagram, c.url.as_ref().or(c.value.as_ref())),
                "whatsapp" => first(&mut row.whatsapp, c.url.as_ref().or(c.value.as_ref())),
                "website" => {
                    let v = c.value.as_ref().or(c.url.as_ref());
                    match classify_website(c) {
                        Some(ContactKind::Website) => first(&mut row.website, v),
                        Some(ContactKind::Instagram) => first(&mut row.instagram, v),
                        Some(ContactKind::Whatsapp) => first(&mut row.whatsapp, v),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if let Some(i) = row.whatsapp.find('?') {
            row.whatsapp.truncate(i);
        }
        row
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub stats: Stats,
    pub saved: usize,
    pub path: std::path::PathBuf,
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
pub enum Event {
    Log(LogLevel, String),
    Lead(LeadRow),
    Stats(Stats),
    Status(String),
    Done(Result<Summary, String>),
}

/// Канал событий в интерфейс. В CLI — пустой (события не отправляются).
#[derive(Clone, Default)]
pub struct Reporter {
    tx: Option<Sender<Event>>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Reporter {
    pub fn new(tx: Sender<Event>, waker: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { tx: Some(tx), waker: Some(waker) }
    }

    pub fn send(&self, e: Event) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(e);
            if let Some(w) = &self.waker {
                w();
            }
        }
    }
}

/// tracing-слой, пересылающий сообщения журнала в интерфейс.
pub struct UiLogLayer {
    pub reporter: Reporter,
}

struct MsgVisitor(String);

impl Visit for MsgVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }
}

impl<S: Subscriber> Layer<S> for UiLogLayer {
    fn on_event(&self, event: &TracingEvent<'_>, _ctx: Context<'_, S>) {
        let level = match *event.metadata().level() {
            Level::ERROR => LogLevel::Error,
            Level::WARN => LogLevel::Warn,
            Level::INFO => LogLevel::Info,
            _ => LogLevel::Debug,
        };
        let mut v = MsgVisitor(String::new());
        event.record(&mut v);
        self.reporter.send(Event::Log(level, v.0));
    }
}
