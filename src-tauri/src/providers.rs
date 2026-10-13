//! Provider windows: a signed-in gemini.google.com / chatgpt.com page
//! in a WebView2 window that we drive with `driver.js`.
//!
//! The window is "parked": DWM-cloaked on the primary monitor, so it's invisible
//! and click-through but still rendered at full size (the page is never throttled
//! as hidden). Sign-in happens in separate, normal windows. The page captures the
//! mic itself; we only auto-allow the microphone for the provider's domain.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_CLOAK, DWMWA_CLOAKED,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_BOTTOM, SWP_NOACTIVATE,
    SWP_NOSIZE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

pub const LABEL: &str = "provider";
/// Every webview must share the same args (they share one browser process).
/// Tauri's defaults plus no GPU: the provider page doesn't need it, and the GPU
/// process alone costs ~250 MB on high-DPI screens.
pub const BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-gpu --disable-gpu-compositing";
const DRIVER: &str = include_str!("driver.js");

/// Selector config for one provider. Patterns are `css:<selector>` or
/// `re:<regex>` (matched case-insensitively against a button's aria-label,
/// title, test ids, tooltip and text).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub url: String,
    pub composer: Vec<String>,
    pub start: Vec<String>,
    pub stop: Vec<String>,
    pub cancel: Vec<String>,
    pub recording: Vec<String>,
    pub busy: Vec<String>,
    pub signed_out: Vec<String>,
    pub exclude: Vec<String>,
    /// Extra popup-closing button patterns (on top of the built-in ones).
    pub dismiss: Vec<String>,
    /// Keyboard shortcut that starts dictation, used if the button isn't found.
    pub start_shortcut: Option<Shortcut>,
    /// Stop by clicking the mic button again when no explicit stop button exists.
    pub toggle_stop: bool,
    /// Re-arm the mic if the site stops listening on a pause while the key is held.
    pub restart_on_silence: bool,
    /// Extra wait after the site reports recording before we say "Listening"
    /// (for sites that show their recording UI before the audio stream is live).
    pub warmup_ms: u64,
    /// Transcript is final once unchanged for this long.
    pub settle_ms: u64,
    /// Give up waiting for any text after this long.
    pub empty_timeout_ms: u64,
    pub timeout_ms: u64,
    /// How to send a prompt and read the answer (command mode, polish).
    /// `None` = this provider can't do it.
    pub ask: Option<AskConfig>,
}

/// Selectors for asking the provider to write something.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AskConfig {
    /// If set, every ask starts by loading this URL (e.g. ChatGPT's
    /// `?temporary-chat=true`), instead of clicking "new chat".
    pub start_url: String,
    pub new_chat: Vec<String>,
    /// Temporary/incognito chat toggle, so asks never land in chat history.
    pub temporary: Vec<String>,
    /// Regex over the page text that's true while temporary mode is on.
    pub temporary_active: String,
    /// Buttons that only exist while temporary mode is on (e.g. "Turn off temporary chat").
    pub temporary_on: Vec<String>,
    /// CSS for code blocks inside an answer. Answers are requested inside one
    /// plain-text code block, which chat apps show character-for-character.
    pub code_block: String,
    pub mode_picker: Vec<String>,
    /// Regex with one capture group: the current model's name, from the picker's label.
    pub mode_current: String,
    /// Menu-item regexes for the fastest / balanced models.
    pub model_fastest: String,
    pub model_balanced: String,
    /// Regexes over the current model name, to tell if a switch is needed.
    pub name_fastest: String,
    pub name_balanced: String,
    pub send: Vec<String>,
    /// CSS selector for the assistant's answer bubbles.
    pub response: String,
    /// Buttons that exist only while an answer is being written.
    pub generating: Vec<String>,
    pub settle_ms: u64,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Shortcut {
    pub key: String,
    pub code: Option<String>,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Gemini first: it's the default provider (best accuracy in practice).
pub fn defaults() -> Vec<ProviderConfig> {
    vec![
        ProviderConfig {
            id: "gemini".into(),
            name: "Gemini".into(),
            url: "https://gemini.google.com/app".into(),
            composer: v(&[
                "rich-textarea .ql-editor",
                ".ql-editor[contenteditable='true']",
                "div[contenteditable='true'][role='textbox']",
            ]),
            start: v(&[
                "re:^dictate",
                "re:^(use )?microphone",
                "re:^(start )?voice (input|typing)",
            ]),
            stop: v(&[
                "re:^stop (dictat|listen|record|microphone|voice)",
                "re:^done$",
            ]),
            cancel: v(&["re:^cancel (dictat|voice|record)"]),
            recording: v(&[
                "re:^stop (dictat|listen|record|microphone|voice)",
                "re:^listening",
            ]),
            busy: vec![],
            signed_out: v(&["re:^sign in$"]),
            exclude: v(&["re:^(start )?live", "re:gemini live", "re:^send"]),
            start_shortcut: Some(Shortcut {
                key: "D".into(),
                code: Some("KeyD".into()),
                ctrl: true,
                shift: true,
                ..Default::default()
            }),
            toggle_stop: true,
            restart_on_silence: true,
            settle_ms: 600,
            empty_timeout_ms: 2_500,
            timeout_ms: 15_000,
            ask: Some(AskConfig {
                new_chat: v(&["re:^new chat$"]),
                temporary: v(&["re:^temporary chat$"]),
                temporary_active: r"\bTemporary chats?\b".into(),
                code_block: "code-block code, pre code, pre".into(),
                mode_picker: v(&["re:^open mode picker"]),
                mode_current: r"currently\s+(.+)$".into(),
                model_fastest: r"Flash-Lite".into(),
                model_balanced: r"^[\d.]*\s*Flash(?!-)\b".into(),
                name_fastest: r"Flash-Lite".into(),
                name_balanced: r"^Flash$".into(),
                send: v(&["css:button[aria-label='Send message']", "re:^send message$"]),
                response: "model-response message-content".into(),
                generating: v(&["re:^stop( response| generating)?$"]),
                settle_ms: 700,
                timeout_ms: 90_000,
                ..Default::default()
            }),
            ..Default::default()
        },
        ProviderConfig {
            id: "chatgpt".into(),
            name: "ChatGPT".into(),
            url: "https://chatgpt.com/".into(),
            composer: v(&[
                "#prompt-textarea",
                "textarea#mobile-composer-prompt",
                "div.ProseMirror[contenteditable='true']",
                "form textarea",
            ]),
            start: v(&[
                "css:button[aria-label='Start dictation']",
                "re:^start dictation",
                "re:^dictat(e|ion)( button)?$",
            ]),
            // "Stop dictation" transcribes into the box; "Transcribe and send" would
            // send the message (blocked by the driver's hard guard regardless).
            stop: v(&[
                "re:^submit dictation",
                "re:^stop dictation$",
                "re:^(done|accept|confirm|finish)( dictation)?$",
                "re:^stop recording$",
            ]),
            cancel: v(&["re:^(cancel|discard) (dictation|recording)"]),
            recording: v(&[
                "re:^submit dictation",
                "re:^(stop|cancel) dictation",
                "re:^stop recording",
                "re:^transcribe",
            ]),
            busy: v(&["re:transcrib", "css:[data-testid*='transcrib' i]"]),
            signed_out: v(&["re:^log in$", "re:^sign up for free$"]),
            exclude: v(&[
                "re:voice mode",
                "re:^voice$",
                "re:^(start|use) voice",
                "re:^send",
            ]),
            toggle_stop: false,
            restart_on_silence: false,
            settle_ms: 350,
            empty_timeout_ms: 12_000,
            timeout_ms: 30_000,
            ask: Some(AskConfig {
                start_url: "https://chatgpt.com/?temporary-chat=true".into(),
                new_chat: v(&["re:^new chat$"]),
                temporary: v(&["re:^temporary chat$"]),
                temporary_on: v(&["re:^turn off temporary chat$"]),
                code_block: "[data-markdown-copy='code-block'] code, pre code, pre".into(),
                send: v(&[
                    "css:button[data-testid='send-button']",
                    "re:^send( prompt| message)?$",
                ]),
                response: "[data-content-search-unit-key$=':assistant'], [data-message-author-role='assistant']"
                    .into(),
                generating: v(&[
                    "re:^stop( streaming| generating)?$",
                    "css:button[data-testid='stop-button']",
                ]),
                settle_ms: 600,
                timeout_ms: 120_000,
                ..Default::default()
            }),
            ..Default::default()
        },
    ]
}

/// Built-in defaults, overridden per provider id by `providers.json` (if the
/// user created one via "Customize selectors").
pub fn load_configs(dir: &Path) -> Vec<ProviderConfig> {
    let mut list = defaults();
    let Ok(raw) = std::fs::read_to_string(dir.join("providers.json")) else {
        return list;
    };
    match serde_json::from_str::<Vec<ProviderConfig>>(&raw) {
        Ok(user) => {
            // Only the built-in providers can be overridden; anything else (e.g. a
            // provider from an older version) is ignored.
            for u in user {
                match list.iter_mut().find(|p| p.id == u.id) {
                    Some(slot) => *slot = u,
                    None => log::warn!("providers.json: unknown provider {:?} ignored", u.id),
                }
            }
        }
        Err(e) => log::warn!("providers.json ignored (invalid JSON): {e}"),
    }
    list
}

/// Which provider the live window belongs to.
pub struct Loaded(pub Mutex<Option<String>>);

fn label_for(id: &str) -> String {
    format!("{LABEL}-{id}")
}

/// The parked window of the currently loaded provider.
pub fn window(app: &AppHandle) -> Option<WebviewWindow> {
    let id = app.state::<Loaded>().0.lock().unwrap().clone()?;
    app.get_webview_window(&label_for(&id))
}

/// Make sure the provider window for `cfg` exists (replacing another provider's).
pub fn ensure(app: &AppHandle, cfg: &ProviderConfig) -> anyhow::Result<WebviewWindow> {
    let loaded = app.state::<Loaded>();
    let label = label_for(&cfg.id);
    let previous = loaded.0.lock().unwrap().clone();
    if previous.as_deref() == Some(cfg.id.as_str()) {
        if let Some(w) = app.get_webview_window(&label) {
            return Ok(w);
        }
    }
    // Only one provider page stays loaded, to keep memory down.
    if let Some(old) = previous.and_then(|id| app.get_webview_window(&label_for(&id))) {
        let _ = old.destroy();
    }
    let w = build_parked(app, &label, &cfg.url, cfg, true)?;
    *loaded.0.lock().unwrap() = Some(cfg.id.clone());
    app.state::<std::sync::Arc<crate::idle::Activity>>()
        .page_loaded();
    Ok(w)
}

/// A parked (cloaked) provider window at `cfg.url`.
fn build_parked(
    app: &AppHandle,
    label: &str,
    url: &str,
    cfg: &ProviderConfig,
    microphone: bool,
) -> anyhow::Result<WebviewWindow> {
    let url: Url = url.parse().context("provider url")?;
    let host = url.host_str().unwrap_or_default().to_string();
    // Created hidden, then parked (cloaked) and shown without activation.
    let w = WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        .title(format!("{} · VillFlow V4", cfg.name))
        .additional_browser_args(BROWSER_ARGS)
        .inner_size(1100.0, 800.0)
        .skip_taskbar(true)
        .focused(false)
        .visible(false)
        .build()?;
    park(&w);
    // Closing the window (after "Show provider window") just parks it again.
    let handle = w.clone();
    w.on_window_event(move |e| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = e {
            api.prevent_close();
            park(&handle);
        }
    });
    if microphone {
        allow_microphone(&w, host);
    }
    Ok(w)
}

/// Which provider the separate writing-assistant window belongs to (when the
/// writer isn't the dictation provider, e.g. dictate with Gemini, write with ChatGPT).
pub struct WriterLoaded(pub Mutex<Option<String>>);

fn writer_label(id: &str) -> String {
    format!("writer-{id}")
}

/// The window to send writing requests to: the dictation window itself when
/// it's the same provider, otherwise a second parked window just for writing.
pub fn ensure_writer(app: &AppHandle, cfg: &ProviderConfig) -> anyhow::Result<WebviewWindow> {
    let dictation = app.state::<Loaded>().0.lock().unwrap().clone();
    if dictation.as_deref() == Some(cfg.id.as_str()) {
        if let Some(w) = window(app) {
            unload_writer(app); // no need for a second page of the same site
            return Ok(w);
        }
    }
    let state = app.state::<WriterLoaded>();
    let previous = state.0.lock().unwrap().clone();
    let label = writer_label(&cfg.id);
    if previous.as_deref() == Some(cfg.id.as_str()) {
        if let Some(w) = app.get_webview_window(&label) {
            return Ok(w);
        }
    }
    if let Some(old) = previous.and_then(|id| app.get_webview_window(&writer_label(&id))) {
        let _ = old.destroy();
    }
    // Start on the provider's temporary-chat page when it has one.
    let start = cfg
        .ask
        .as_ref()
        .map(|a| a.start_url.clone())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| cfg.url.clone());
    let w = build_parked(app, &label, &start, cfg, false)?;
    *state.0.lock().unwrap() = Some(cfg.id.clone());
    Ok(w)
}

pub fn writer_window(app: &AppHandle) -> Option<WebviewWindow> {
    let id = app.state::<WriterLoaded>().0.lock().unwrap().clone()?;
    app.get_webview_window(&writer_label(&id))
}

pub fn unload_writer(app: &AppHandle) {
    if let Some(w) = writer_window(app) {
        let _ = w.destroy();
    }
    *app.state::<WriterLoaded>().0.lock().unwrap() = None;
}

/// Unload the provider pages (frees their memory).
pub fn unload(app: &AppHandle) {
    if let Some(w) = window(app) {
        let _ = w.destroy();
    }
    *app.state::<Loaded>().0.lock().unwrap() = None;
    unload_writer(app);
}

/// Open a normal, on-screen window for signing in to `cfg`. It shares cookies
/// with the parked dictation window; when it closes, the parked page reloads so
/// it picks up the new session.
pub fn open_signin(app: &AppHandle, cfg: &ProviderConfig) -> anyhow::Result<WebviewWindow> {
    let label = format!("signin-{}", cfg.id);
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(w);
    }
    let url: Url = cfg.url.parse().context("provider url")?;
    let host = url.host_str().unwrap_or_default().to_string();
    let w = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(url))
        .title(format!("Sign in to {} · VillFlow V4", cfg.name))
        .additional_browser_args(BROWSER_ARGS)
        .inner_size(1000.0, 780.0)
        .center()
        .build()?;
    allow_microphone(&w, host);
    let app2 = app.clone();
    let id = cfg.id.clone();
    let url = cfg.url.clone();
    w.on_window_event(move |e| {
        if let tauri::WindowEvent::Destroyed = e {
            let loaded = app2.state::<Loaded>();
            if loaded.0.lock().unwrap().as_deref() == Some(id.as_str()) {
                if let (Some(p), Ok(u)) = (window(&app2), url.parse()) {
                    let _ = p.navigate(u);
                    app2.state::<std::sync::Arc<crate::idle::Activity>>()
                        .page_loaded();
                }
            }
            let _ = tauri::Emitter::emit_to(&app2, "main", "vf://signin-closed", id.clone());
        }
    });
    Ok(w)
}

fn hwnd_of(w: &WebviewWindow) -> Option<HWND> {
    w.hwnd().ok().map(|h| HWND(h.0 as *mut _))
}

fn set_cloaked(hwnd: HWND, on: bool) {
    let v: i32 = on as i32;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CLOAK,
            &v as *const i32 as *const _,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut v: u32 = 0;
    unsafe {
        DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut v as *mut u32 as *mut _, 4).is_ok()
            && v != 0
    }
}

/// Hide the provider window without really hiding it: it stays "shown" on a real
/// monitor (so the page renders at full size, at the right DPI, and is never
/// throttled as a background tab) but is DWM-cloaked, so it's invisible, takes no
/// clicks, and is kept out of the taskbar and Alt+Tab.
pub fn park(w: &WebviewWindow) {
    let Some(hwnd) = hwnd_of(w) else { return };
    set_cloaked(hwnd, true);
    let _ = w.set_skip_taskbar(true);
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            (ex | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize),
        );
        // Always on the primary monitor (whose top-left is 0,0): a window left on a
        // monitor that's since been unplugged would render at the wrong DPI.
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            0,
            0,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
    // Show through tao (so it tracks visibility and keeps the webview sized);
    // built with focused(false), so this doesn't activate it.
    let _ = w.show();
}

/// Bring the parked window on-screen (troubleshooting).
pub fn present(w: &WebviewWindow) {
    if let Some(hwnd) = hwnd_of(w) {
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !(WS_EX_TOOLWINDOW.0 as isize));
        }
        set_cloaked(hwnd, false);
    }
    let _ = w.set_skip_taskbar(false);
    let _ = w.center();
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
}

pub fn is_parked(w: &WebviewWindow) -> bool {
    hwnd_of(w).map(is_cloaked).unwrap_or(true)
}

/// Grant microphone access to the provider's own domain without a prompt (the
/// default WebView2 prompt would be invisible while the window is parked).
fn allow_microphone(w: &WebviewWindow, host: String) {
    // "gemini.google.com" -> also allow "*.google.com" (e.g. accounts redirects).
    let base: String = {
        let parts: Vec<&str> = host.split('.').collect();
        parts[parts.len().saturating_sub(2)..].join(".")
    };
    let res = w.with_webview(move |pw| unsafe {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
            COREWEBVIEW2_PERMISSION_STATE_ALLOW,
        };
        use webview2_com::PermissionRequestedEventHandler;

        let core = match pw.controller().CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                log::error!("CoreWebView2 unavailable: {e}");
                return;
            }
        };
        let handler = PermissionRequestedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
            args.PermissionKind(&mut kind)?;
            if kind != COREWEBVIEW2_PERMISSION_KIND_MICROPHONE {
                return Ok(());
            }
            let mut uri = windows::core::PWSTR::null();
            args.Uri(&mut uri)?;
            let uri = webview2_com::take_pwstr(uri);
            let req_host = uri
                .parse::<Url>()
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_default();
            if req_host == base || req_host.ends_with(&format!(".{base}")) {
                args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW)?;
                log::info!("microphone allowed for {req_host}");
            } else {
                log::warn!("microphone request from {req_host} left to default");
            }
            Ok(())
        }));
        let mut token = 0i64;
        if let Err(e) = core.add_PermissionRequested(&handler, &mut token) {
            log::error!("add_PermissionRequested failed: {e}");
        }
    });
    if let Err(e) = res {
        log::error!("with_webview failed: {e}");
    }
}

/// Tell WebView2 how hard to hold on to memory: `low` while idle (it trims caches
/// and can page out the renderer), `normal` while dictating.
pub fn set_memory_low(w: &WebviewWindow, low: bool) {
    let _ = w.with_webview(move |pw| unsafe {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW,
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
        };
        use windows::core::Interface;
        let Ok(core) = pw.controller().CoreWebView2() else {
            return;
        };
        if let Ok(c19) = core.cast::<ICoreWebView2_19>() {
            let level = if low {
                COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
            } else {
                COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
            };
            let _ = c19.SetMemoryUsageTargetLevel(level);
        }
    });
}

/// Run a driver method (`start`, `tick`, `stop`, `poll`, `cancel`, `status`,
/// `inspect`, `ask*`) in the provider page and return its JSON result.
pub async fn call(app: &AppHandle, cfg: &ProviderConfig, method: &str) -> anyhow::Result<Value> {
    call_with(app, cfg, method, &[]).await
}

/// [`call`] with arguments (passed as JSON, so any text is safe to embed).
pub async fn call_with(
    app: &AppHandle,
    cfg: &ProviderConfig,
    method: &str,
    args: &[Value],
) -> anyhow::Result<Value> {
    let w = window(app).ok_or_else(|| anyhow!("provider window not loaded"))?;
    call_on(&w, cfg, method, args).await
}

/// Run a driver method in a specific provider window.
pub async fn call_on(
    w: &WebviewWindow,
    cfg: &ProviderConfig,
    method: &str,
    args: &[Value],
) -> anyhow::Result<Value> {
    let cfg_json = serde_json::to_string(cfg)?;
    let js = format!(
        "(function(){{try{{const CFG={cfg_json};const api=(function(){{{DRIVER}\n}})();return api[{m}](...{a});}}\
         catch(e){{return {{ok:false,error:'driver: '+String(e&&e.message||e)}};}}}})()",
        m = serde_json::to_string(method)?,
        a = serde_json::to_string(args)?,
    );
    log::trace!("call {method}");
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let tx = Mutex::new(Some(tx));
    w.eval_with_callback(js, move |res| {
        if let Some(tx) = tx.lock().unwrap().take() {
            let _ = tx.send(res);
        }
    })?;
    let raw = tokio::time::timeout(Duration::from_secs(3), rx)
        .await
        .map_err(|_| anyhow!("provider page did not respond"))??;
    log::trace!("call {method} -> {} bytes", raw.len());
    let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    if value.is_null() {
        // Page mid-navigation, or script blocked.
        anyhow::bail!("provider page not ready");
    }
    Ok(value)
}
