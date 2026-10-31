//! Запуск задачи парсинга — общий для CLI и оконного интерфейса.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use tracing::{error, info};

use crate::browser::{ChromeOptions, Remote};
use crate::events::{Event, Reporter, Summary};
use crate::filter::Filters;
use crate::parser::{Parser, ParserOptions, Seen, Stats};
use crate::writer::{self, Format, WriterOptions};

#[derive(Debug, Clone)]
pub struct JobConfig {
    pub urls: Vec<String>,
    /// Подписи для статуса («Костанай · барбершоп»), по одной на URL; можно пустой
    pub labels: Vec<String>,
    pub output: PathBuf,
    pub format: Format,
    pub filters: Filters,
    pub chrome: ChromeOptions,
    pub parser: ParserOptions,
    pub writer: WriterOptions,
}

pub async fn run_job(cfg: JobConfig, reporter: Reporter, cancel: Arc<AtomicBool>) -> Result<Summary> {
    if cfg.urls.is_empty() {
        bail!("не указано ни одного URL");
    }
    if cfg.filters.is_active() {
        info!("Фильтры: {}", cfg.filters.describe());
    }

    let mut sink = writer::open(&cfg.output, cfg.format, cfg.writer.clone())?;

    reporter.send(Event::Status("запуск браузера".into()));
    let mut stats = Stats::default();
    let mut seen = Seen::default();
    let parse_result: Result<()> = async {
        let mut remote = Remote::start(&cfg.chrome).await?;
        let total = cfg.urls.len();
        for (i, url) in cfg.urls.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            info!("Парсинг ссылки [{}/{}] {url}", i + 1, total);
            let label = match (cfg.labels.get(i).filter(|l| !l.is_empty()), total > 1) {
                (Some(l), true) => format!("{l} ({}/{total})", i + 1),
                (Some(l), false) => l.clone(),
                (None, true) => format!("Ссылка {}/{total}", i + 1),
                (None, false) => "Парсинг".to_string(),
            };
            let mut p = Parser::new(
                &mut remote,
                &cfg.parser,
                &cfg.filters,
                sink.as_mut(),
                cancel.clone(),
                reporter.clone(),
                &mut seen,
            )
            .with_stats(stats, label);
            let res = p.parse_url(url).await;
            stats = p.stats;
            if let Err(e) = res {
                error!("Ошибка при парсинге {url}: {e:#}");
            }
        }
        reporter.send(Event::Status("закрытие браузера".into()));
        remote.close().await;
        Ok(())
    }
    .await;

    reporter.send(Event::Status("сохранение файла".into()));
    let saved = sink.finish().context("ошибка сохранения результата")?;
    parse_result?;

    let cancelled = cancel.load(Ordering::Relaxed);
    info!(
        "Готово. Просмотрено: {}, подошло: {} (в файле: {}), отсеяно фильтрами: {}, повторов: {}, ошибок: {}. Файл: {}",
        stats.scanned,
        stats.written,
        saved,
        stats.filtered,
        stats.duplicates,
        stats.errors,
        cfg.output.display()
    );
    Ok(Summary { stats, saved, path: cfg.output.clone(), cancelled })
}
