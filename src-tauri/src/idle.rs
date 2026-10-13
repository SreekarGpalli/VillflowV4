//! Keeps the parked provider page cheap while you're not dictating:
//! - after a few seconds idle, WebView2 is told to use as little memory as possible;
//! - optionally the page is unloaded entirely after N idle minutes (reloads on the next press);
//! - a page that has sat idle for hours is reloaded so the site's session/app version stays fresh.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

use crate::providers;
use crate::store::Store;

const LOW_MEMORY_AFTER: Duration = Duration::from_secs(20);
const REFRESH_AFTER: Duration = Duration::from_secs(3 * 60 * 60);

pub struct Activity {
    last: Mutex<Instant>,
    /// Last time the page was (re)loaded.
    loaded: Mutex<Instant>,
    busy: AtomicBool,
    low: AtomicBool,
    /// Bumped by every dictation session, so delayed follow-ups (the "mic still
    /// on?" check) can tell a newer session has started and stand down.
    generation: AtomicU64,
}

impl Default for Activity {
    fn default() -> Self {
        Self {
            last: Mutex::new(Instant::now()),
            loaded: Mutex::new(Instant::now()),
            busy: AtomicBool::new(false),
            low: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        }
    }
}

impl Activity {
    /// Whether a dictation session is running right now.
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// A dictation session is starting. Returns its generation.
    pub fn begin(&self, app: &AppHandle) -> u64 {
        self.busy.store(true, Ordering::SeqCst);
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.full_speed(app);
        generation
    }

    /// Make sure the page runs at full speed (memory level "normal").
    pub fn full_speed(&self, app: &AppHandle) {
        *self.last.lock().unwrap() = Instant::now();
        self.low.store(false, Ordering::SeqCst);
        // Unconditionally: a reload keeps the webview's level, so the flag alone
        // can't tell whether it's still "low". The call is a cheap message.
        for w in providers::window(app)
            .into_iter()
            .chain(providers::writer_window(app))
        {
            providers::set_memory_low(&w, false);
        }
    }

    /// True while no newer session than `generation` has started.
    pub fn is_current(&self, generation: u64) -> bool {
        !self.busy.load(Ordering::SeqCst) && self.generation.load(Ordering::SeqCst) == generation
    }

    pub fn end(&self) {
        *self.last.lock().unwrap() = Instant::now();
        self.busy.store(false, Ordering::SeqCst);
    }

    /// A (re)load just happened (new provider, reload, sign-in).
    pub fn page_loaded(&self) {
        *self.loaded.lock().unwrap() = Instant::now();
        *self.last.lock().unwrap() = Instant::now();
        self.low.store(false, Ordering::SeqCst);
    }
}

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            let act = app.state::<Arc<Activity>>();
            if act.busy.load(Ordering::SeqCst) {
                continue;
            }
            let Some(w) = providers::window(&app) else {
                continue;
            };
            let idle = act.last.lock().unwrap().elapsed();
            let unload_after = app.state::<Arc<Store>>().settings().free_memory_after_mins;

            if unload_after > 0 && idle >= Duration::from_secs(unload_after as u64 * 60) {
                log::info!("idle {}s: unloading provider page", idle.as_secs());
                providers::unload(&app);
                continue;
            }
            if act.loaded.lock().unwrap().elapsed() >= REFRESH_AFTER
                && idle >= Duration::from_secs(60)
            {
                log::info!("refreshing provider page after long idle");
                let id = app.state::<providers::Loaded>().0.lock().unwrap().clone();
                let url = id.and_then(|id| {
                    app.state::<crate::Configs>()
                        .0
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|p| p.id == id)
                        .map(|p| p.url.clone())
                });
                if let Some(Ok(url)) = url.map(|u| u.parse()) {
                    let _ = w.navigate(url);
                }
                act.page_loaded();
                continue;
            }
            if idle >= LOW_MEMORY_AFTER && !act.low.swap(true, Ordering::SeqCst) {
                providers::set_memory_low(&w, true);
                if let Some(writer) = providers::writer_window(&app) {
                    providers::set_memory_low(&writer, true);
                }
            }
        }
    });
}

/// Right after signing in to Windows the network is often not up yet, so the
/// provider pages can load as error pages. Check them a few times over the first
/// few minutes and reload any page that isn't usable, so VillFlow is ready by the
/// time you first press the hotkey.
pub fn check_pages_after_start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        for wait in [8u64, 15, 30, 60, 120] {
            tokio::time::sleep(Duration::from_secs(wait)).await;
            let settings = app.state::<Arc<Store>>().settings();
            let configs = app.state::<crate::Configs>().0.lock().unwrap().clone();
            let find = |id: &str| configs.iter().find(|c| c.id == id).cloned();
            // (window, config, the URL it should be showing)
            let mut pages = Vec::new();
            if let (Some(w), Some(cfg)) = (providers::window(&app), find(&settings.provider)) {
                let url = cfg.url.clone();
                pages.push((w, cfg, url));
            }
            if let Some(w) = providers::writer_window(&app) {
                let id = app
                    .state::<providers::WriterLoaded>()
                    .0
                    .lock()
                    .unwrap()
                    .clone();
                if let Some(cfg) = id.as_deref().and_then(find) {
                    let url = cfg
                        .ask
                        .as_ref()
                        .map(|a| a.start_url.clone())
                        .filter(|u| !u.is_empty())
                        .unwrap_or_else(|| cfg.url.clone());
                    pages.push((w, cfg, url));
                }
            }
            let mut all_ready = true;
            for (w, cfg, url) in pages {
                let ready = match providers::call_on(&w, &cfg, "status", &[]).await {
                    Ok(s) => {
                        s["composer"].as_bool() == Some(true)
                            || s["signed_out"].as_bool() == Some(true)
                    }
                    Err(_) => false,
                };
                if ready {
                    continue;
                }
                all_ready = false;
                // Never pull the page out from under a dictation in progress.
                if app.state::<Arc<Activity>>().is_busy() {
                    continue;
                }
                log::warn!("{} page isn't ready after start; reloading", cfg.id);
                if let Ok(u) = url.parse() {
                    let _ = w.navigate(u);
                }
            }
            if all_ready {
                log::info!("provider pages ready");
                break;
            }
        }
    });
}
