//! Командная строка (запуск с аргументами).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use clap::Parser as ClapParser;
use tracing::warn;

use crate::browser::ChromeOptions;
use crate::events::Reporter;
use crate::filter::{ContactKind, Filters, WithoutMode};
use crate::job::{JobConfig, run_job};
use crate::lang::LangFilter;
use crate::parser::ParserOptions;
use crate::writer::{Encoding, Format, WriterOptions};

/// Парсер 2GIS на Rust: собирает организации из выдачи 2GIS через Chrome
/// и сохраняет в CSV / XLSX / JSON, с фильтрами для поиска лидов.
#[derive(ClapParser, Debug)]
#[command(name = "parser-2gis", version, about, long_about = None)]
#[command(after_long_help = "Без аргументов программа открывает оконный интерфейс.")]
#[command(after_help = "Примеры:
  # Барбершопы Костаная без сайта, от 10 отзывов
  parser-2gis -i 'https://2gis.kz/kostanay/search/барбершоп' -o leads.xlsx --no-website --min-reviews 10

  # Нет ни Instagram, ни WhatsApp
  parser-2gis -i URL -o out.csv --no-instagram --no-whatsapp

  # Нет Instagram ИЛИ нет WhatsApp (хотя бы одного), но телефон обязателен
  parser-2gis -i URL -o out.csv --no-instagram --no-whatsapp --without-mode any --with phone")]
pub struct Cli {
    /// URL выдачи 2GIS (поиск, рубрика, карточка фирмы или «в здании»)
    #[arg(short = 'i', long = "url", num_args = 1.., required_unless_present_any = ["urls_file", "query"])]
    urls: Vec<String>,

    /// Файл со списком URL (по одному на строку)
    #[arg(long, value_name = "FILE")]
    urls_file: Option<PathBuf>,

    /// Что искать (вместо ссылок), можно несколько: -q барбершоп -q "салон красоты"
    #[arg(short = 'q', long = "query", value_name = "ТЕКСТ")]
    query: Vec<String>,

    /// Город для --query (код из 2GIS: kostanay, almaty, nur_sultan…)
    #[arg(long, value_name = "CODE", conflicts_with = "country")]
    city: Option<String>,

    /// Искать по всем городам страны (kz, ru, by, kg, uz…)
    #[arg(long, value_name = "CC")]
    country: Option<String>,

    /// Путь до результирующего файла
    #[arg(short = 'o', long = "output-path", value_name = "PATH")]
    output: PathBuf,

    /// Формат файла (по умолчанию — по расширению)
    #[arg(short = 'f', long, value_enum)]
    format: Option<Format>,

    // ---------------------------------------------------------- фильтры
    /// Только без сайта
    #[arg(long, help_heading = "Фильтры")]
    no_website: bool,

    /// Только без Instagram
    #[arg(long, help_heading = "Фильтры")]
    no_instagram: bool,

    /// Только без WhatsApp
    #[arg(long, help_heading = "Фильтры")]
    no_whatsapp: bool,

    /// Только без указанных контактов (через запятую)
    #[arg(long, value_enum, value_delimiter = ',', value_name = "KIND,...", help_heading = "Фильтры")]
    without: Vec<ContactKind>,

    /// all — нет НИ ОДНОГО из перечисленных; any — нет ХОТЯ БЫ ОДНОГО
    #[arg(long, value_enum, default_value_t = WithoutMode::All, help_heading = "Фильтры")]
    without_mode: WithoutMode,

    /// Обязательно должны быть указанные контакты (через запятую)
    #[arg(long, value_enum, value_delimiter = ',', value_name = "KIND,...", help_heading = "Фильтры")]
    with: Vec<ContactKind>,

    /// Минимум отзывов
    #[arg(long, value_name = "N", help_heading = "Фильтры")]
    min_reviews: Option<i64>,

    /// Максимум отзывов
    #[arg(long, value_name = "N", help_heading = "Фильтры")]
    max_reviews: Option<i64>,

    /// Минимальный рейтинг (записи без рейтинга отсеиваются)
    #[arg(long, value_name = "X", help_heading = "Фильтры")]
    min_rating: Option<f64>,

    /// Максимальный рейтинг
    #[arg(long, value_name = "X", help_heading = "Фильтры")]
    max_rating: Option<f64>,

    /// Считать сайтом любую ссылку в поле «сайт» (по умолчанию instagram/wa.me/taplink и т.п. сайтом не считаются)
    #[arg(long, help_heading = "Фильтры")]
    strict_website: bool,

    /// Не отбрасывать остановки/улицы/парковки при активных фильтрах
    #[arg(long, help_heading = "Фильтры")]
    include_non_firms: bool,

    /// Язык названия: any, not-kazakh (отсеять казахские), russian, kazakh
    #[arg(long, value_enum, default_value_t = LangFilter::Any, help_heading = "Фильтры")]
    lang: LangFilter,

    /// Не отсеивать филиалы одной сети (по умолчанию пишется один филиал на сеть)
    #[arg(long, help_heading = "Фильтры")]
    all_branches: bool,

    // ---------------------------------------------------------- браузер
    /// Путь до Chrome/Chromium (по умолчанию ищется автоматически)
    #[arg(long, value_name = "PATH", help_heading = "Браузер")]
    chrome_path: Option<PathBuf>,

    /// Скрыть браузер (headless)
    #[arg(long, help_heading = "Браузер")]
    headless: bool,

    /// Не отключать картинки, карты и шрифты
    #[arg(long, help_heading = "Браузер")]
    show_images: bool,

    /// Запустить окно развёрнутым
    #[arg(long, help_heading = "Браузер")]
    maximized: bool,

    /// Дополнительный аргумент запуска Chrome (можно несколько раз)
    #[arg(long = "chrome-arg", value_name = "ARG", allow_hyphen_values = true, help_heading = "Браузер")]
    chrome_args: Vec<String>,

    /// Лимит памяти V8 (МБ)
    #[arg(long, default_value_t = 4096, value_name = "MB", help_heading = "Браузер")]
    memory_limit: u64,

    // ---------------------------------------------------------- парсер
    /// Максимум ЗАПИСАННЫХ записей с одного URL (0 — без лимита)
    #[arg(long, default_value_t = 0, value_name = "N", help_heading = "Парсер")]
    max_records: usize,

    /// Задержка между кликами по записям, мс
    #[arg(long, default_value_t = 0, value_name = "MS", help_heading = "Парсер")]
    delay_ms: u64,

    /// Не пропускать URL, вернувшие «Не найдено»
    #[arg(long, help_heading = "Парсер")]
    no_skip_404: bool,

    /// Сборщик мусора в браузере каждые N страниц (0 — выкл)
    #[arg(long, default_value_t = 10, value_name = "N", help_heading = "Парсер")]
    gc_pages: usize,

    // ---------------------------------------------------------- таблица
    /// Колонок на сущность (Телефон 1..N), 1–5
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=5), help_heading = "CSV/XLSX")]
    columns_per_entity: u8,

    /// Не добавлять колонку «Рубрики»
    #[arg(long, help_heading = "CSV/XLSX")]
    no_rubrics: bool,

    /// Не добавлять комментарии к телефонам, e-mail и т.п.
    #[arg(long, help_heading = "CSV/XLSX")]
    no_comments: bool,

    /// Не удалять пустые колонки
    #[arg(long, help_heading = "CSV/XLSX")]
    keep_empty_columns: bool,

    /// Не удалять дубликаты
    #[arg(long, help_heading = "CSV/XLSX")]
    keep_duplicates: bool,

    /// Разделитель для Рубрик и Часов работы
    #[arg(long, default_value = "; ", help_heading = "CSV/XLSX")]
    join_char: String,

    /// Кодировка CSV
    #[arg(long, value_enum, default_value_t = Encoding::Utf8Sig, help_heading = "CSV/XLSX")]
    encoding: Encoding,

    /// Не печатать каждую запись
    #[arg(short, long)]
    quiet: bool,
}


pub fn main() -> i32 {
    let cli = Cli::parse();

    let filter = std::env::var("RUST_LOG")
        .unwrap_or_else(|_| "info,chromiumoxide=off,tungstenite=off".into());
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .init();

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    match rt.block_on(run(cli)) {
        Ok(()) => 0,
        Err(e) => {
            tracing::error!("{e:#}");
            1
        }
    }
}

async fn run(cli: Cli) -> Result<()> {

    let mut urls = cli.urls.clone();
    if let Some(f) = &cli.urls_file {
        let text = std::fs::read_to_string(f).with_context(|| format!("не удалось прочитать {f:?}"))?;
        urls.extend(
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from),
        );
    }
    let mut labels: Vec<String> = urls.iter().map(|_| String::new()).collect();
    if !cli.query.is_empty() {
        let cities: Vec<&crate::cities::City> = match (&cli.city, &cli.country) {
            (Some(code), _) => crate::cities::all().iter().filter(|c| &c.code == code).collect(),
            (None, Some(cc)) => crate::cities::all().iter().filter(|c| c.country_code == cc.to_lowercase()).collect(),
            (None, None) => bail!("для --query укажите --city или --country"),
        };
        if cities.is_empty() {
            bail!("город/страна не найдены в списке 2GIS");
        }
        for city in cities {
            for q in &cli.query {
                urls.push(city.search_url(q));
                labels.push(format!("{} · {q}", city.name));
            }
        }
    }
    if urls.is_empty() {
        bail!("не указано ни одного URL");
    }

    let format = match cli.format.or_else(|| Format::from_path(&cli.output)) {
        Some(f) => f,
        None => bail!("не удалось определить формат по расширению — укажите -f csv|xlsx|json"),
    };

    let mut without = cli.without.clone();
    for (flag, kind) in [
        (cli.no_website, ContactKind::Website),
        (cli.no_instagram, ContactKind::Instagram),
        (cli.no_whatsapp, ContactKind::Whatsapp),
    ] {
        if flag && !without.contains(&kind) {
            without.push(kind);
        }
    }
    let filters = Filters {
        without,
        without_mode: cli.without_mode,
        with: cli.with.clone(),
        min_reviews: cli.min_reviews,
        max_reviews: cli.max_reviews,
        min_rating: cli.min_rating,
        max_rating: cli.max_rating,
        strict_website: cli.strict_website,
        include_non_firms: cli.include_non_firms,
        lang: cli.lang,
        one_per_org: !cli.all_branches,
    };

    let chrome = ChromeOptions {
        binary_path: cli.chrome_path.clone(),
        headless: cli.headless,
        disable_images: !cli.show_images,
        start_maximized: cli.maximized,
        memory_limit: cli.memory_limit,
        extra_args: cli.chrome_args.clone(),
    };
    let popts = ParserOptions {
        skip_404_response: !cli.no_skip_404,
        delay_between_clicks: cli.delay_ms,
        max_records: if cli.max_records == 0 { usize::MAX } else { cli.max_records },
        gc_pages_interval: cli.gc_pages,
        verbose: !cli.quiet,
    };
    let wopts = WriterOptions {
        encoding: cli.encoding,
        add_rubrics: !cli.no_rubrics,
        add_comments: !cli.no_comments,
        columns_per_entity: cli.columns_per_entity as usize,
        remove_empty_columns: !cli.keep_empty_columns,
        remove_duplicates: !cli.keep_duplicates,
        join_char: cli.join_char.clone(),
    };

    let cfg = JobConfig {
        urls,
        labels,
        output: cli.output.clone(),
        format,
        filters,
        chrome,
        parser: popts,
        writer: wopts,
    };

    // Ctrl+C: аккуратно дописываем файл, а не теряем собранное
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                warn!("Прерывание — сохраняю собранное…");
                cancel.store(true, Ordering::Relaxed);
            }
        });
    }

    run_job(cfg, Reporter::default(), cancel).await?;
    Ok(())
}
