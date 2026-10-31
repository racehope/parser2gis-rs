//! Парсеры страниц 2GIS: поисковая выдача, карточка фирмы, "в здании".

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use regex::Regex;
use serde_json::{Value, json};
use tracing::{debug, error, info, warn};

use crate::browser::{Captured, Remote};
use crate::events::{Event, LeadRow, Reporter};
use crate::filter::Filters;
use crate::model::CatalogItem;
use crate::writer::Sink;

#[derive(Debug, Clone)]
pub struct ParserOptions {
    pub skip_404_response: bool,
    pub delay_between_clicks: u64,
    pub max_records: usize,
    pub gc_pages_interval: usize,
    pub verbose: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub scanned: usize,
    pub written: usize,
    pub filtered: usize,
    pub duplicates: usize,
    pub errors: usize,
}

/// Что уже встречалось за всю задачу (сквозь все ссылки и города).
#[derive(Debug, Default)]
pub struct Seen {
    branches: HashSet<String>,
    orgs: HashSet<String>,
}

#[derive(PartialEq)]
enum Flow {
    Continue,
    Stop,
}

static FIRM_ID_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"/firm/(\d+)").unwrap());

const SEARCH_LINKS_JS: &str = r#"
(() => {
  const re = /.*\/(firm|station)\/.*\?stat=([a-zA-Z0-9%]+)/;
  const out = [];
  for (const a of document.querySelectorAll('a[href]')) {
    const h = a.getAttribute('href');
    const m = h && h.match(re);
    if (!m) continue;
    try { atob(decodeURIComponent(m[2])); out.push(h); } catch (e) {}
  }
  return [...new Set(out)];
})()
"#;

const INSIDE_LINKS_JS: &str = r#"
(() => {
  const re = /^\/[^\/]+\/firm\/[^\/]+$/;
  const out = [];
  for (const a of document.querySelectorAll('a[href]')) {
    const h = a.getAttribute('href');
    if (h && re.test(h)) out.push(h);
  }
  return [...new Set(out)];
})()
"#;

const PAGES_JS: &str = r#"
(() => {
  const re = /.*\/search\/.*\/page\/(\d+)/;
  const out = {};
  for (const a of document.querySelectorAll('a[href]')) {
    const h = a.getAttribute('href');
    const m = h && h.match(re);
    if (m) out[m[1]] = h;
  }
  return out;
})()
"#;

const INITIAL_STATE_JS: &str = r#"
(() => {
  try {
    const p = window.initialState && window.initialState.data
      && window.initialState.data.entity && window.initialState.data.entity.profile;
    if (!p) return null;
    const v = Object.values(p)[0];
    return v ? { data: v.data, meta: v.meta } : null;
  } catch (e) { return null; }
})()
"#;

fn click_js(href: &str) -> String {
    let h = serde_json::to_string(href).unwrap();
    format!(
        r#"(() => {{
  const h = {h};
  const el = Array.from(document.querySelectorAll('a[href]')).find(a => a.getAttribute('href') === h);
  if (!el) return false;
  el.scrollIntoView({{ block: 'center', behavior: 'instant' }});
  el.click();
  return true;
}})()"#
    )
}

pub struct Parser<'a> {
    remote: &'a mut Remote,
    opts: &'a ParserOptions,
    filters: &'a Filters,
    sink: &'a mut dyn Sink,
    cancel: Arc<AtomicBool>,
    pub stats: Stats,
    seen: &'a mut Seen,
    collected_for_url: usize,
    reporter: Reporter,
    label: String,
    page: usize,
}

impl<'a> Parser<'a> {
    pub fn new(
        remote: &'a mut Remote,
        opts: &'a ParserOptions,
        filters: &'a Filters,
        sink: &'a mut dyn Sink,
        cancel: Arc<AtomicBool>,
        reporter: Reporter,
        seen: &'a mut Seen,
    ) -> Self {
        Self {
            remote,
            opts,
            filters,
            sink,
            cancel,
            stats: Stats::default(),
            seen,
            collected_for_url: 0,
            reporter,
            label: String::new(),
            page: 1,
        }
    }

    /// Начальная статистика (накопленная с предыдущих URL) — чтобы в интерфейсе
    /// счётчики шли сквозь все ссылки.
    pub fn with_stats(mut self, stats: Stats, label: String) -> Self {
        self.stats = stats;
        self.label = label;
        self
    }

    fn status(&self, extra: &str) {
        let mut s = self.label.clone();
        if self.page > 1 {
            s.push_str(&format!(" · страница {}", self.page));
        }
        if !extra.is_empty() {
            s.push_str(" · ");
            s.push_str(extra);
        }
        self.reporter.send(Event::Status(s));
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub async fn parse_url(&mut self, url: &str) -> Result<()> {
        self.collected_for_url = 0;
        self.page = 1;
        self.status("загрузка");
        let firm = Regex::new(r"^https?://2gis\.[^/]+(/[^/]+)?/firm/.*").unwrap();
        let inside = Regex::new(r"^https?://2gis\.[^/]+/[^/]+/inside/.*").unwrap();
        if firm.is_match(url) {
            self.parse_firm(url).await
        } else if inside.is_match(url) {
            self.parse_inside(url).await
        } else {
            self.parse_search(url).await
        }
    }

    // ------------------------------------------------------------ общие шаги

    async fn check_document(&self, not_found_msg: &str) -> bool {
        match self.remote.document_status(Duration::from_secs(5)).await {
            None => {
                error!("Ошибка получения ответа сервера.");
                false
            }
            Some(404) => {
                warn!("Сервер вернул сообщение \"{not_found_msg}\".");
                !self.opts.skip_404_response
            }
            Some(_) => true,
        }
    }

    /// Ждём, пока завершатся все запросы страницы к 2GIS.
    async fn wait_requests_finished(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        // Небольшая пауза, чтобы клик/навигация успели породить запросы
        tokio::time::sleep(Duration::from_millis(150)).await;
        loop {
            let open = self.remote.eval("(window.__p2gOpen || 0)").await.ok().and_then(|v| v.as_i64());
            if open == Some(0) || self.cancelled() {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                warn!("Не дождались завершения запросов страницы, продолжаем.");
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn get_links(&self, js: &str) -> Vec<String> {
        match self.remote.eval(js).await {
            Ok(Value::Array(a)) => a.into_iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            _ => vec![],
        }
    }

    async fn click(&self, href: &str) -> bool {
        matches!(self.remote.eval(&click_js(href)).await, Ok(Value::Bool(true)))
    }

    /// Обработать JSON-документ карточки: фильтр + запись.
    fn handle_doc(&mut self, doc: &Value) -> Flow {
        if let Some(err) = doc.pointer("/meta/error") {
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("неизвестная ошибка");
            error!("Сервер ответил ошибкой: {msg}");
            self.stats.errors += 1;
            return Flow::Continue;
        }
        if doc.pointer("/meta/code").and_then(|c| c.as_i64()) != Some(200) {
            error!("Некорректный документ каталога, пропуск позиции.");
            self.stats.errors += 1;
            return Flow::Continue;
        }
        let Some(raw) = doc.pointer("/result/items/0") else {
            error!("Пустой документ каталога, пропуск позиции.");
            self.stats.errors += 1;
            return Flow::Continue;
        };
        let item: CatalogItem = match serde_json::from_value(raw.clone()) {
            Ok(i) => i,
            Err(e) => {
                error!("Ошибка разбора карточки: {e}");
                self.stats.errors += 1;
                return Flow::Continue;
            }
        };

        let name = item.display_name();

        // Та же точка уже попадалась (другой запрос / город / страница)
        if !self.seen.branches.insert(item.branch_id().to_string()) {
            self.stats.duplicates += 1;
            debug!("повтор > {name}");
            return Flow::Continue;
        }
        self.stats.scanned += 1;

        let mut verdict = self.filters.check(&item);
        if verdict.is_ok() && self.filters.one_per_org {
            if let Some(org) = item.org_key() {
                if self.seen.orgs.contains(&org) {
                    verdict = Err("другой филиал этой сети уже записан".into());
                }
            }
        }

        match verdict {
            Ok(()) => {
                if let Err(e) = self.sink.write(raw, &item) {
                    error!("Ошибка во время записи: {e:#}");
                    self.stats.errors += 1;
                    return Flow::Continue;
                }
                self.stats.written += 1;
                self.collected_for_url += 1;
                if let Some(org) = item.org_key() {
                    self.seen.orgs.insert(org);
                }
                self.reporter.send(Event::Lead(LeadRow::from_item(&item)));
                if self.opts.verbose {
                    info!("Парсинг [{}] > {name}", self.stats.written);
                }
            }
            Err(reason) => {
                self.stats.filtered += 1;
                if self.opts.verbose {
                    info!("  пропуск > {name} ({reason})");
                } else {
                    debug!("пропуск > {name} ({reason})");
                }
            }
        }

        self.reporter.send(Event::Stats(self.stats));

        if self.collected_for_url >= self.opts.max_records {
            info!("Спарсено максимально разрешенное количество записей с данного URL.");
            return Flow::Stop;
        }
        Flow::Continue
    }

    /// Кликнуть по организации и забрать перехваченный JSON карточки.
    async fn process_link(&mut self, href: &str) -> Flow {
        let flow = self.process_link_inner(href).await;
        self.reporter.send(Event::Stats(self.stats));
        flow
    }

    /// Ожидание ответа карточки с проверкой отмены.
    async fn wait_item(&mut self, total: Duration) -> Option<Captured> {
        let deadline = tokio::time::Instant::now() + total;
        while tokio::time::Instant::now() < deadline {
            if self.cancelled() {
                return None;
            }
            if let Some(c) = self.remote.wait_item(Duration::from_millis(250)).await {
                return Some(c);
            }
        }
        None
    }

    async fn process_link_inner(&mut self, href: &str) -> Flow {
        // Филиал уже видели — не тратим время на клик
        if let Some(id) = FIRM_ID_RE.captures(href).map(|c| c[1].to_string()) {
            if self.seen.branches.contains(&id) {
                self.stats.duplicates += 1;
                return Flow::Continue;
            }
        }
        let mut body = None;
        for attempt in 1..=3 {
            if self.cancelled() {
                return Flow::Stop;
            }
            self.remote.drain();
            if !self.click(href).await {
                debug!("Ссылка {href} пропала из DOM");
                break;
            }
            if self.opts.delay_between_clicks > 0 {
                tokio::time::sleep(Duration::from_millis(self.opts.delay_between_clicks)).await;
            }
            match self.wait_item(Duration::from_secs(30)).await {
                Some(Captured::Done(id)) => match self.remote.response_body(&id).await {
                    Ok(b) => {
                        body = Some(b);
                        break;
                    }
                    Err(e) => debug!("попытка {attempt}: {e}"),
                },
                Some(Captured::Failed(e)) => debug!("попытка {attempt}: запрос упал: {e}"),
                None => debug!("попытка {attempt}: нет ответа"),
            }
        }

        let Some(body) = body else {
            if self.cancelled() {
                return Flow::Stop;
            }
            error!("Данные не получены, пропуск позиции.");
            self.stats.errors += 1;
            return Flow::Continue;
        };
        match serde_json::from_str::<Value>(&body) {
            Ok(doc) => self.handle_doc(&doc),
            Err(_) => {
                error!("Сервер вернул некорректный JSON документ, пропуск позиции.");
                self.stats.errors += 1;
                Flow::Continue
            }
        }
    }

    // ------------------------------------------------------------ поиск

    async fn available_pages(&self) -> BTreeMap<usize, String> {
        match self.remote.eval(PAGES_JS).await {
            Ok(Value::Object(m)) => m
                .into_iter()
                .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_str()?.to_string())))
                .collect(),
            _ => BTreeMap::new(),
        }
    }

    /// Переход на страницу `n`. Ссылка должна быть в DOM — иначе
    /// антибот 2GIS перекинет на первую страницу.
    async fn go_page(&self, n: usize) -> Option<usize> {
        let pages = self.available_pages().await;
        let href = pages.get(&n)?;
        self.click(href).await.then_some(n)
    }

    /// Ссылки текущей страницы, которых ещё не было.
    ///
    /// Ждём, пока DOM обновится (набор ссылок станет отличаться от прошлой
    /// страницы), и отбрасываем уже виденные. В оригинале страница считалась
    /// «не обновившейся», если на ней была хоть одна знакомая ссылка — и
    /// выдача обрывалась, когда организация повторялась на двух страницах.
    async fn unique_search_links(&self, visited: &mut HashSet<String>, prev_page: &mut HashSet<String>) -> Vec<String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let links = self.get_links(SEARCH_LINKS_JS).await;
            let set: HashSet<String> = links.iter().cloned().collect();
            if !links.is_empty() && set != *prev_page {
                *prev_page = set;
                let fresh: Vec<String> = links.into_iter().filter(|l| visited.insert(l.clone())).collect();
                return fresh;
            }
            if tokio::time::Instant::now() >= deadline || self.cancelled() {
                return vec![];
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn parse_search(&mut self, url: &str) -> Result<()> {
        let page_re = Regex::new(r"(?i)/page/(\d+)").unwrap();
        let mut walk_to: Option<usize> = page_re
            .captures(url)
            .and_then(|c| c[1].parse().ok());
        let base_url = page_re.replace_all(url, "").to_string();

        self.remote.navigate(&base_url).await?;
        if !self.check_document("Точных совпадений нет / Не найдено").await {
            return Ok(());
        }

        let mut visited = HashSet::new();
        let mut prev_page = HashSet::new();
        let mut current = 1usize;

        loop {
            if self.cancelled() {
                return Ok(());
            }
            self.wait_requests_finished().await;
            let links = self.unique_search_links(&mut visited, &mut prev_page).await;

            self.page = current;
            if walk_to.is_none() {
                debug!("Страница {current}: {} ссылок", links.len());
                self.status(&format!("{} организаций на странице", links.len()));
                for href in links {
                    if self.process_link(&href).await == Flow::Stop {
                        return Ok(());
                    }
                }
            }

            if self.opts.gc_pages_interval > 0 && current % self.opts.gc_pages_interval == 0 {
                debug!("Запуск сборщика мусора.");
                let _ = self.remote.eval("'gc' in window && window.gc()").await;
            }

            let next = match walk_to {
                Some(target) => {
                    let pages = self.available_pages().await;
                    pages
                        .keys()
                        .copied()
                        .filter(|&k| k > current)
                        .min_by_key(|&k| k.abs_diff(target))
                        .unwrap_or(current + 1)
                }
                None => current + 1,
            };

            match self.go_page(next).await {
                Some(n) => current = n,
                None => break, // конец выдачи
            }
            if let Some(w) = walk_to {
                if w <= current {
                    walk_to = None;
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ фирма

    async fn parse_firm(&mut self, url: &str) -> Result<()> {
        self.remote.navigate(url).await?;
        if !self.check_document("Организация не найдена").await {
            return Ok(());
        }
        self.wait_requests_finished().await;

        let state = self.remote.eval(INITIAL_STATE_JS).await?;
        let (Some(data), meta) = (state.get("data").filter(|d| !d.is_null()), state.get("meta")) else {
            warn!("Данные организации не найдены.");
            return Ok(());
        };
        let mut meta = meta.cloned().unwrap_or(json!({}));
        if meta.get("code").is_none() {
            meta["code"] = json!(200);
        }
        let doc = json!({ "result": { "items": [data] }, "meta": meta });
        self.handle_doc(&doc);
        Ok(())
    }

    // ------------------------------------------------------------ "в здании"

    async fn parse_inside(&mut self, url: &str) -> Result<()> {
        self.remote.navigate(url).await?;
        if !self.check_document("Точных совпадений нет / Не найдено").await {
            return Ok(());
        }

        let mut visited: HashSet<String> = HashSet::new();
        loop {
            if self.cancelled() {
                return Ok(());
            }
            self.wait_requests_finished().await;

            // Новые ссылки (ленивая подгрузка списка), ждём до 5 секунд
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            let links = loop {
                let fresh: Vec<String> = self
                    .get_links(INSIDE_LINKS_JS)
                    .await
                    .into_iter()
                    .filter(|l| !visited.contains(l))
                    .collect();
                if !fresh.is_empty() || tokio::time::Instant::now() >= deadline {
                    break fresh;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            };
            if links.is_empty() {
                break;
            }
            visited.extend(links.iter().cloned());

            for href in links {
                if self.process_link(&href).await == Flow::Stop {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}
