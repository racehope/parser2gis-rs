//! Обёртка над Chrome DevTools Protocol: запуск браузера, перехват ответов
//! `catalog.api.2gis.*/items/byid`, выполнение JS, блокировка мусорных запросов.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::{
    BlockPattern, EventLoadingFailed, EventLoadingFinished, EventRequestWillBeSent,
    EventResponseReceived, GetResponseBodyParams, RequestId, ResourceType, SetBlockedUrLsParams,
};
use chromiumoxide::cdp::browser_protocol::page::{
    AddScriptToEvaluateOnNewDocumentParams, NavigateParams,
};
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::page::Page;
use futures::StreamExt;
use regex::Regex;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct ChromeOptions {
    pub binary_path: Option<PathBuf>,
    pub headless: bool,
    pub disable_images: bool,
    pub start_maximized: bool,
    pub memory_limit: u64,
    pub extra_args: Vec<String>,
}

/// Результат перехваченного запроса карточки организации.
#[derive(Debug)]
pub enum Captured {
    Done(RequestId),
    Failed(String),
}

pub struct Remote {
    browser: Browser,
    page: Page,
    handler_task: JoinHandle<()>,
    events_task: JoinHandle<()>,
    rx: mpsc::UnboundedReceiver<Captured>,
    doc_status: Arc<Mutex<Option<i64>>>,
    _profile: tempfile::TempDir,
}

const ITEM_RESPONSE_PATTERN: &str = r"^https://catalog\.api\.2gis\.[^/]+/.*/items/byid";

/// Счётчик незавершённых запросов к 2GIS (XHR + fetch), чтобы ждать
/// окончания подгрузки страницы.
const COUNTER_SCRIPT: &str = r#"
(function () {
  if (window.__p2gPatched) return;
  window.__p2gPatched = true;
  window.__p2gOpen = 0;
  var re = /^https?:\/\/[^\/]*2gis\.[a-z]+/i;
  var origOpen = XMLHttpRequest.prototype.open;
  var origSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.open = function (method, url) {
    this.__p2gTrack = re.test(String(url));
    return origOpen.apply(this, arguments);
  };
  XMLHttpRequest.prototype.send = function () {
    if (this.__p2gTrack) {
      window.__p2gOpen++;
      var done = false;
      this.addEventListener('loadend', function () {
        if (!done) { done = true; window.__p2gOpen--; }
      });
    }
    return origSend.apply(this, arguments);
  };
  if (window.fetch) {
    var origFetch = window.fetch;
    window.fetch = function (input) {
      var u = typeof input === 'string' ? input : (input && input.url) || String(input);
      if (!re.test(u)) return origFetch.apply(this, arguments);
      window.__p2gOpen++;
      return origFetch.apply(this, arguments).finally(function () { window.__p2gOpen--; });
    };
  }
})();
"#;

const HIDE_WEBDRIVER_SCRIPT: &str =
    "Object.defineProperty(navigator, 'webdriver', { get: () => undefined });";

fn blocked_patterns(extended: bool) -> Vec<&'static str> {
    let mut v = vec![
        "https://favorites.api.2gis.*/*",
        "https://2gis.*/_/log",
        "https://2gis.*/_/metrics",
        "https://google-analytics.com/*",
        "https://www.google-analytics.com/*",
        "https://counter.yadro.ru/*",
        "https://www.tns-counter.ru/*",
        "https://mc.yandex.ru/*",
        "https://catalog.api.2gis.ru/3.0/ads/*",
        "https://d-assets.2gis.*/privacyPolicyBanner*.js",
        "https://vk.com/*",
    ];
    if extended {
        v.extend([
            "https://d-assets.2gis.*/fonts/*",
            "https://mapgl.2gis.*/api/fonts/*",
            "https://tile*.maps.2gis.*/*",
            "https://s*.bss.2gis.*/*",
            "https://styles.api.2gis.*/*",
            "https://video-pr.api.2gis.*/*",
            "https://api.photo.2gis.*/*",
            "https://market-backend.api.2gis.*/*",
            "https://traffic*.edromaps.2gis.*/*",
            "https://disk.2gis.*/styles/*",
        ]);
    }
    v
}

/// Поиск Chromium-браузера в стандартных местах Windows:
/// Chrome → Edge (есть в любой Windows 10/11) → Яндекс.Браузер → Chromium.
/// На других ОС полагаемся на автопоиск chromiumoxide.
pub fn detect_browser() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let roots: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData", "ProgramW6432"]
        .iter()
        .filter_map(|k| env(k))
        .collect();
    let rel = [
        r"Google\Chrome\Application\chrome.exe",
        r"Microsoft\Edge\Application\msedge.exe",
        r"Yandex\YandexBrowser\Application\browser.exe",
        r"Chromium\Application\chrome.exe",
    ];
    for r in rel {
        for root in &roots {
            let p = root.join(r);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// chromiumoxide форматирует аргумент как `--{key}`, так что `--foo=bar`
/// превратился бы в `----foo=bar` и молча игнорировался бы Chrome.
fn cdp_arg(a: &str) -> String {
    a.trim_start_matches('-').to_string()
}

impl Remote {
    pub async fn start(opts: &ChromeOptions) -> Result<Self> {
        let profile = tempfile::tempdir().context("не удалось создать временный профиль")?;

        // Все аргументы Chrome собираем сами: chromiumoxide сам дописывает "--"
        // перед ключом, поэтому ведущие дефисы срезаем (см. `cdp_arg`).
        let mut args: Vec<String> = vec![
            "--no-default-browser-check".into(),
            "--no-first-run".into(),
            "--disable-fre".into(),
            "--remote-allow-origins=*".into(),
            "--disable-blink-features=AutomationControlled".into(),
            // Не душить таймеры, когда окно браузера свёрнуто/перекрыто
            "--disable-background-timer-throttling".into(),
            "--disable-backgrounding-occluded-windows".into(),
            "--disable-renderer-backgrounding".into(),
            format!("--js-flags=--expose-gc --max-old-space-size={}", opts.memory_limit),
        ];
        if opts.headless {
            args.push("--disable-gpu".into());
        }
        if opts.disable_images {
            args.push("--blink-settings=imagesEnabled=false".into());
        }
        if opts.start_maximized {
            args.push("--start-maximized".into());
        }
        args.extend(opts.extra_args.iter().cloned());

        let mut builder = BrowserConfig::builder()
            .disable_default_args()
            .user_data_dir(profile.path())
            .request_timeout(Duration::from_secs(120))
            .launch_timeout(Duration::from_secs(60))
            .no_sandbox()
            .args(args.iter().map(|a| cdp_arg(a)));

        builder = if opts.headless {
            builder.new_headless_mode()
        } else {
            builder.with_head().viewport(None)
        };
        if !opts.start_maximized && !opts.headless {
            builder = builder.window_size(1280, 900);
        }
        if let Some(p) = opts.binary_path.clone().or_else(detect_browser) {
            debug!("Браузер: {}", p.display());
            builder = builder.chrome_executable(p);
        }

        let config = builder.build().map_err(|_| {
            anyhow!("не найден Chrome, Edge или Яндекс.Браузер — укажите путь к браузеру вручную")
        })?;

        debug!("Запуск Chrome");
        let (browser, mut handler) = Browser::launch(config)
            .await
            .context("не удалось запустить Chrome")?;

        // CDP-хендлер должен крутиться всё время жизни браузера.
        // Ошибки разбора отдельных сообщений (новые события Chrome) не фатальны.
        let handler_task = tokio::spawn(async move {
            while let Some(ev) = handler.next().await {
                if let Err(e) = ev {
                    debug!("CDP handler: {e}");
                }
            }
        });

        let page = browser.new_page("about:blank").await.context("не удалось открыть вкладку")?;

        // Убираем "Headless" из UA и следы webdriver
        if let Ok(ua) = page.user_agent().await {
            if ua.contains("Headless") {
                page.set_user_agent(ua.replace("Headless", "")).await.ok();
            }
        }

        let (tx, rx) = mpsc::unbounded_channel();
        let doc_status = Arc::new(Mutex::new(None));
        let events_task = Self::spawn_event_loop(&page, tx, doc_status.clone()).await?;

        let remote = Self {
            browser,
            page,
            handler_task,
            events_task,
            rx,
            doc_status,
            _profile: profile,
        };

        remote.add_start_script(HIDE_WEBDRIVER_SCRIPT).await?;
        remote.add_start_script(COUNTER_SCRIPT).await?;

        let patterns: Vec<BlockPattern> = blocked_patterns(opts.disable_images)
            .into_iter()
            .map(|p| BlockPattern::new(p, true))
            .collect();
        if let Err(e) = remote
            .page
            .execute(SetBlockedUrLsParams::builder().url_patterns(patterns).build())
            .await
        {
            warn!("Не удалось заблокировать служебные запросы (не критично): {e}");
        }

        Ok(remote)
    }

    async fn spawn_event_loop(
        page: &Page,
        tx: mpsc::UnboundedSender<Captured>,
        doc_status: Arc<Mutex<Option<i64>>>,
    ) -> Result<JoinHandle<()>> {
        let mut sent = page.event_listener::<EventRequestWillBeSent>().await?;
        let mut recv = page.event_listener::<EventResponseReceived>().await?;
        let mut fin = page.event_listener::<EventLoadingFinished>().await?;
        let mut fail = page.event_listener::<EventLoadingFailed>().await?;
        let re = Regex::new(ITEM_RESPONSE_PATTERN).unwrap();

        Ok(tokio::spawn(async move {
            let mut tracked: HashSet<RequestId> = HashSet::new();
            loop {
                tokio::select! {
                    Some(ev) = sent.next() => {
                        if ev.r#type == Some(ResourceType::Preflight) { continue; }
                        if re.is_match(&ev.request.url) {
                            tracked.insert(ev.request_id.clone());
                        }
                    }
                    Some(ev) = recv.next() => {
                        if ev.r#type == ResourceType::Document {
                            let mut s = doc_status.lock().unwrap();
                            if s.is_none() { *s = Some(ev.response.status); }
                        }
                    }
                    Some(ev) = fin.next() => {
                        if tracked.remove(&ev.request_id) {
                            let _ = tx.send(Captured::Done(ev.request_id.clone()));
                        }
                    }
                    Some(ev) = fail.next() => {
                        if tracked.remove(&ev.request_id) {
                            let _ = tx.send(Captured::Failed(ev.error_text.clone()));
                        }
                    }
                    else => break,
                }
            }
        }))
    }

    pub async fn add_start_script(&self, source: &str) -> Result<()> {
        self.page
            .execute(AddScriptToEvaluateOnNewDocumentParams::new(source))
            .await?;
        Ok(())
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        *self.doc_status.lock().unwrap() = None;
        let params = NavigateParams::builder()
            .url(url)
            .referrer("https://google.com")
            .build()
            .map_err(|e| anyhow!(e))?;
        self.page.goto(params).await.with_context(|| format!("ошибка перехода на {url}"))?;
        Ok(())
    }

    /// HTTP-статус основного документа после `navigate` (ждёт до `timeout`).
    pub async fn document_status(&self, timeout: Duration) -> Option<i64> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(s) = *self.doc_status.lock().unwrap() {
                return Some(s);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn eval(&self, expr: &str) -> Result<Value> {
        let params = EvaluateParams::builder()
            .expression(expr)
            .return_by_value(true)
            .await_promise(true)
            .build()
            .map_err(|e| anyhow!(e))?;
        let res = self.page.evaluate_expression(params).await?;
        Ok(res.value().cloned().unwrap_or(Value::Null))
    }

    /// Сбросить ранее перехваченные (устаревшие) ответы.
    pub fn drain(&mut self) {
        while self.rx.try_recv().is_ok() {}
    }

    pub async fn wait_item(&mut self, timeout: Duration) -> Option<Captured> {
        tokio::time::timeout(timeout, self.rx.recv()).await.ok().flatten()
    }

    pub async fn response_body(&self, id: &RequestId) -> Result<String> {
        let mut last_err = None;
        for _ in 0..10 {
            match self.page.execute(GetResponseBodyParams::new(id.clone())).await {
                Ok(r) => {
                    let r = r.result;
                    return if r.base64_encoded {
                        let bytes = base64::engine::general_purpose::STANDARD.decode(r.body)?;
                        Ok(String::from_utf8_lossy(&bytes).into_owned())
                    } else {
                        Ok(r.body)
                    };
                }
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
            }
        }
        bail!("тело ответа недоступно: {:?}", last_err)
    }

    pub async fn close(mut self) {
        let _ = self.browser.close().await;
        let _ = tokio::time::timeout(Duration::from_secs(5), self.browser.wait()).await;
        self.events_task.abort();
        self.handler_task.abort();
    }
}
