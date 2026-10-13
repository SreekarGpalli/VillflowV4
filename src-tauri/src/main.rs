// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assist;
mod dictation;
mod hotkeys;
mod idle;
mod inject;
mod overlay;
mod providers;
mod store;

use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder};

use providers::{Loaded, ProviderConfig};
use store::{Entry, Settings, Stats, Store};

/// Live provider selector configs (defaults + user overrides).
pub struct Configs(pub Mutex<Vec<ProviderConfig>>);

/// Why the global hotkey isn't working, if it isn't (shown in the UI).
pub struct HotkeyError(pub Mutex<Option<String>>);

type Res<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[derive(Serialize)]
struct ProviderInfo {
    id: String,
    name: String,
    url: String,
}

#[derive(Serialize)]
struct AppState {
    settings: Settings,
    stats: Stats,
    providers: Vec<ProviderInfo>,
    loaded_provider: Option<String>,
    hotkey_error: Option<String>,
    data_dir: String,
    version: &'static str,
}

#[tauri::command]
fn get_state(app: AppHandle, store: State<Arc<Store>>, configs: State<Configs>) -> Res<AppState> {
    Ok(AppState {
        settings: store.settings(),
        stats: store.stats().map_err(err)?,
        providers: configs
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|p| ProviderInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                url: p.url.clone(),
            })
            .collect(),
        loaded_provider: app.state::<Loaded>().0.lock().unwrap().clone(),
        hotkey_error: app.state::<HotkeyError>().0.lock().unwrap().clone(),
        data_dir: store.dir().display().to_string(),
        version: env!("CARGO_PKG_VERSION"),
    })
}

// Commands that create or touch windows are async: a sync command runs on the
// main thread inside a WebView2 callback, and building a webview there deadlocks.
#[tauri::command]
async fn save_settings(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    mut settings: Settings,
) -> Res<()> {
    let before = store.settings();
    config_for(&app, &settings.provider)?;
    hotkeys::validate(&settings.hotkey, &settings.command_hotkey).map_err(err)?;
    if settings.hotkey != before.hotkey || settings.command_hotkey != before.command_hotkey {
        hotkeys::set_combos(&settings.hotkey, &settings.command_hotkey).map_err(err)?;
    }
    // Tidy the dictionary: trimmed, no blanks, no duplicate "from" entries.
    let mut seen = std::collections::HashSet::new();
    settings.dictionary.retain_mut(|d| {
        d.from = d.from.trim().to_string();
        d.to = d.to.trim().to_string();
        !d.from.is_empty() && !d.to.is_empty() && seen.insert(d.from.to_lowercase())
    });
    if settings.launch_at_login != before.launch_at_login {
        set_launch_at_login(settings.launch_at_login).map_err(err)?;
    }
    if !(settings.writer.is_empty() || settings.writer == "same") {
        let w = config_for(&app, &settings.writer)?;
        if w.ask.is_none() {
            return Err(format!("{} can't run commands", w.name));
        }
    }
    let provider_changed = settings.provider != before.provider;
    let writer_changed = settings.writer != before.writer
        || settings.command_hotkey.is_empty() != before.command_hotkey.is_empty()
        || settings.polish != before.polish;
    store.save_settings(settings).map_err(err)?;
    if provider_changed {
        refresh_tray(&app);
        load_active_provider(&app);
    } else if writer_changed {
        load_writer(&app, &store.settings());
    }
    // Keep any open settings window in sync, whoever changed the settings.
    let _ = tauri::Emitter::emit_to(&app, "main", "vf://settings", ());
    Ok(())
}

/// Stop the global hotkeys from firing while a new one is being recorded.
#[tauri::command]
fn hotkeys_pause(paused: bool) {
    hotkeys::pause(if paused { 20_000 } else { 0 });
}

#[tauri::command]
fn history(
    store: State<Arc<Store>>,
    query: String,
    kind: Option<String>,
    limit: i64,
    offset: i64,
) -> Res<Vec<Entry>> {
    store
        .history_of(kind.as_deref().unwrap_or(""), &query, limit, offset)
        .map_err(err)
}

#[tauri::command]
fn delete_entry(store: State<Arc<Store>>, id: i64) -> Res<()> {
    store.delete(id).map_err(err)
}

#[tauri::command]
fn clear_history(store: State<Arc<Store>>) -> Res<()> {
    store.clear_history().map_err(err)
}

#[tauri::command]
fn copy_text(text: String) -> Res<()> {
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_text(text))
        .map_err(err)
}

fn config_for(app: &AppHandle, id: &str) -> Res<ProviderConfig> {
    app.state::<Configs>()
        .0
        .lock()
        .unwrap()
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .ok_or_else(|| format!("Unknown provider \"{id}\""))
}

/// Only one provider page is kept loaded, so checking any other provider would
/// silently replace the active one's page. Refuse instead.
fn active_only(app: &AppHandle, id: &str) -> Res<ProviderConfig> {
    if app.state::<Arc<Store>>().settings().provider != id {
        return Err("Not the active provider. Click Use to switch to it".into());
    }
    config_for(app, id)
}

/// Show the provider page on-screen so the user can sign in.
#[tauri::command]
async fn provider_open(app: AppHandle, id: String) -> Res<()> {
    let cfg = config_for(&app, &id)?;
    providers::open_signin(&app, &cfg).map_err(err)?;
    Ok(())
}

/// Simulate holding a hotkey for `seconds` (the "Try it" button, and tests).
/// `command: true` presses the command-mode hotkey instead of dictation.
#[tauri::command]
async fn test_dictation(
    store: State<'_, Arc<Store>>,
    seconds: f64,
    command: Option<bool>,
    dry_run: Option<bool>,
    selection: Option<String>,
) -> Res<()> {
    use hotkeys::{HotkeyEvent, Kind};
    if dry_run == Some(true) {
        *dictation::DRY_RUN.lock().unwrap() = Some(selection.filter(|s| !s.is_empty()));
    }
    let kind = if command == Some(true) {
        Kind::Command
    } else {
        Kind::Dictate
    };
    let toggle = store.settings().trigger == store::TriggerMode::Toggle;
    hotkeys::simulate(HotkeyEvent::Down(kind));
    tokio::time::sleep(std::time::Duration::from_secs_f64(
        seconds.clamp(0.05, 120.0),
    ))
    .await;
    hotkeys::simulate(HotkeyEvent::Up(kind));
    if toggle {
        hotkeys::simulate(HotkeyEvent::Down(kind));
        hotkeys::simulate(HotkeyEvent::Up(kind));
    }
    Ok(())
}

/// Run a writing request directly (no microphone, no hotkey, no pasting): the
/// same prompt/send/read path command mode uses. For tests and diagnostics.
#[tauri::command]
async fn debug_write(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    instruction: String,
    selection: Option<String>,
    polish: Option<bool>,
) -> Res<assist::Written> {
    let settings = store.settings();
    let dictation = config_for(&app, &settings.provider)?;
    let writer = if settings.writer.is_empty() || settings.writer == "same" {
        dictation
    } else {
        config_for(&app, &settings.writer)?
    };
    let req = if polish == Some(true) {
        assist::Request::Polish {
            dictation: &instruction,
        }
    } else if let Some(sel) = selection.as_deref().filter(|s| !s.is_empty()) {
        assist::Request::Edit {
            instruction: &instruction,
            selection: sel,
        }
    } else {
        assist::Request::Generate {
            instruction: &instruction,
        }
    };
    assist::write(
        &app,
        &writer,
        req,
        settings.writing_model,
        &settings.dictionary,
    )
    .await
    .map_err(|e| format!("{e:#}"))
}

/// Show the parked dictation window itself (troubleshooting).
#[tauri::command]
async fn provider_show(app: AppHandle) -> Res<()> {
    let w = providers::window(&app).ok_or("No provider is loaded yet")?;
    providers::present(&w);
    Ok(())
}

#[tauri::command]
async fn provider_hide(app: AppHandle) {
    if let Some(w) = providers::window(&app) {
        providers::park(&w);
    }
}

/// Load (if needed) and report whether a provider page looks signed in and usable:
/// the dictation provider, or the separate writing assistant.
#[tauri::command]
async fn provider_status(app: AppHandle, id: String) -> Res<Value> {
    let settings = app.state::<Arc<Store>>().settings();
    let cfg = config_for(&app, &id)?;
    let (w, writer) = if settings.provider == id {
        (providers::ensure(&app, &cfg).map_err(err)?, false)
    } else if settings.writer == id {
        (providers::ensure_writer(&app, &cfg).map_err(err)?, true)
    } else {
        return Err("Not in use".into());
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let res = providers::call_on(&w, &cfg, "status", &[])
            .await
            .map(|mut s| {
                // The writer only needs the message box, not the mic button.
                if writer {
                    let ok = s["composer"].as_bool() == Some(true)
                        && s["signed_out"].as_bool() != Some(true);
                    s["ready"] = json!(ok);
                }
                s["role"] = json!(if writer { "writer" } else { "dictation" });
                s["visible"] = json!(!providers::is_parked(&w));
                s
            });
        match res {
            Ok(s)
                if s["ready"].as_bool() == Some(true)
                    || s["signed_out"].as_bool() == Some(true) =>
            {
                return Ok(s);
            }
            Ok(s) if std::time::Instant::now() >= deadline => return Ok(s),
            Err(e) if std::time::Instant::now() >= deadline => return Err(err(e)),
            _ => tokio::time::sleep(std::time::Duration::from_millis(700)).await,
        }
    }
}

/// Dump the buttons/editors the driver can see (for fixing selectors).
#[tauri::command]
async fn provider_inspect(app: AppHandle, id: String) -> Res<Value> {
    let cfg = active_only(&app, &id)?;
    providers::ensure(&app, &cfg).map_err(err)?;
    providers::call(&app, &cfg, "inspect").await.map_err(err)
}

/// Write providers.json (from current configs) for editing and open it.
#[tauri::command]
fn customize_selectors(app: AppHandle, store: State<Arc<Store>>) -> Res<String> {
    let path = store.dir().join("providers.json");
    if !path.exists() {
        let list = app.state::<Configs>().0.lock().unwrap().clone();
        std::fs::write(&path, serde_json::to_vec_pretty(&list).map_err(err)?).map_err(err)?;
    }
    std::process::Command::new("notepad.exe")
        .arg(&path)
        .spawn()
        .map_err(err)?;
    Ok(path.display().to_string())
}

#[tauri::command]
fn reload_selectors(app: AppHandle, store: State<Arc<Store>>) -> usize {
    let list = providers::load_configs(store.dir());
    let n = list.len();
    *app.state::<Configs>().0.lock().unwrap() = list;
    refresh_tray(&app);
    n
}

#[tauri::command]
fn reset_selectors(app: AppHandle, store: State<Arc<Store>>) -> Res<()> {
    let path = store.dir().join("providers.json");
    if path.exists() {
        std::fs::remove_file(&path).map_err(err)?;
    }
    *app.state::<Configs>().0.lock().unwrap() = providers::defaults();
    refresh_tray(&app);
    Ok(())
}

#[tauri::command]
fn open_data_dir(store: State<Arc<Store>>) -> Res<()> {
    std::process::Command::new("explorer.exe")
        .arg(store.dir())
        .spawn()
        .map_err(err)?;
    Ok(())
}

fn set_launch_at_login(on: bool) -> anyhow::Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;
    // Development builds share the settings of the installed app; they must never
    // point the user's sign-in entry at themselves.
    if cfg!(debug_assertions) {
        log::info!("debug build: leaving the launch-at-login entry alone");
        return Ok(());
    }
    let (key, _) = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")?;
    if on {
        let exe = std::env::current_exe()?;
        key.set_value("VillFlowV4", &format!("\"{}\" --background", exe.display()))?;
    } else {
        let _ = key.delete_value("VillFlowV4");
    }
    Ok(())
}

/// The settings UI is created on demand and destroyed on close, so it costs no
/// memory while you're just dictating.
fn show_main(app: &AppHandle) {
    show_main_at(app, "");
}

/// Open the settings window on a given page ("providers", "history"…).
pub fn show_main_at(app: &AppHandle, view: &str) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        if !view.is_empty() {
            let _ = tauri::Emitter::emit_to(app, "main", "vf://goto", view);
        }
        return;
    }
    let page = if view.is_empty() {
        "index.html".to_string()
    } else {
        format!("index.html#{view}")
    };
    // Paint the window in the page's own background color from the start, so it
    // doesn't flash white before the UI loads in dark mode.
    let background = if windows_dark_mode() {
        tauri::window::Color(0x14, 0x16, 0x15, 0xff)
    } else {
        tauri::window::Color(0xf4, 0xf5, 0xf4, 0xff)
    };
    let built = WebviewWindowBuilder::new(app, "main", WebviewUrl::App(page.into()))
        .title("VillFlow V4")
        .additional_browser_args(providers::BROWSER_ARGS)
        .inner_size(1000.0, 720.0)
        .min_inner_size(760.0, 520.0)
        .background_color(background)
        .center()
        .build();
    if let Err(e) = built {
        log::error!("failed to open main window: {e}");
    }
}

/// Whether Windows apps are set to dark mode.
fn windows_dark_mode() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("AppsUseLightTheme"))
        .map(|light| light == 0)
        .unwrap_or(false)
}

fn load_active_provider(app: &AppHandle) {
    let settings = app.state::<Arc<Store>>().settings();
    let id = settings.provider.clone();
    if let Ok(cfg) = config_for(app, &id) {
        if let Err(e) = providers::ensure(app, &cfg) {
            log::error!("failed to load provider {id}: {e}");
        }
    }
    load_writer(app, &settings);
}

/// Preload a separate writing-assistant page (e.g. ChatGPT while dictating
/// with Gemini) so the first command doesn't wait for it to load.
fn load_writer(app: &AppHandle, settings: &Settings) {
    let separate = !settings.writer.is_empty()
        && settings.writer != "same"
        && settings.writer != settings.provider;
    if !separate || (settings.command_hotkey.is_empty() && !settings.polish) {
        providers::unload_writer(app);
        return;
    }
    match config_for(app, &settings.writer) {
        Ok(cfg) if cfg.ask.is_some() => {
            if let Err(e) = providers::ensure_writer(app, &cfg) {
                log::error!("failed to load writer {}: {e}", cfg.id);
            }
        }
        _ => providers::unload_writer(app),
    }
}

fn tray_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let current = app.state::<Arc<Store>>().settings().provider;
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(
        app,
        "open",
        "Open VillFlow V4",
        true,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    for p in app.state::<Configs>().0.lock().unwrap().iter() {
        let item = CheckMenuItem::with_id(
            app,
            format!("use:{}", p.id),
            &p.name,
            true,
            p.id == current,
            None::<&str>,
        )?;
        menu.append(&item)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?)?;
    Ok(menu)
}

fn refresh_tray(app: &AppHandle) {
    if let (Some(tray), Ok(menu)) = (app.tray_by_id("main"), tray_menu(app)) {
        let _ = tray.set_menu(Some(menu));
    }
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "open" => show_main(app),
        "quit" => app.exit(0),
        other => {
            if let Some(pid) = other.strip_prefix("use:") {
                let store = app.state::<Arc<Store>>();
                let mut s = store.settings();
                s.provider = pid.to_string();
                if let Err(e) = store.save_settings(s) {
                    log::error!("save settings: {e}");
                }
                refresh_tray(app);
                // Off the main thread: building a webview inside an event callback can deadlock.
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    load_active_provider(&app);
                    let _ = tauri::Emitter::emit_to(&app, "main", "vf://settings", ());
                });
            }
        }
    }
}

/// Release builds have no console, so show startup failures in a message box.
fn fatal(msg: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    log::error!("{msg}");
    let text: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "VillFlow V4".encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_ICONERROR | MB_OK,
        );
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    // Dump the built-in provider selectors (used by tests/driver-test and as a
    // starting point for providers.json).
    if std::env::args().any(|a| a == "--print-providers") {
        println!(
            "{}",
            serde_json::to_string_pretty(&providers::defaults()).unwrap_or_default()
        );
        return;
    }
    // Debug builds: `--test-deliver <paste|type|copy> <text>` delivers into the
    // focused window after 2 s (for testing output modes against real apps).
    #[cfg(debug_assertions)]
    {
        let args: Vec<String> = std::env::args().collect();
        if let Some(i) = args.iter().position(|a| a == "--test-deliver") {
            let mode = match args.get(i + 1).map(String::as_str) {
                Some("type") => inject::OutputMode::Type,
                Some("copy") => inject::OutputMode::Copy,
                _ => inject::OutputMode::Paste,
            };
            let text = args
                .get(i + 2)
                .cloned()
                .unwrap_or_default()
                .replace("\\n", "\n");
            std::thread::sleep(std::time::Duration::from_secs(2));
            match inject::deliver(&text, mode, true) {
                Ok(()) => println!("delivered"),
                Err(e) => println!("failed: {e:#}"),
            }
            return;
        }
    }
    let background = std::env::args().any(|a| a == "--background");
    let signin_all = std::env::args().any(|a| a == "--signin-all");

    let store = match Store::open() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            fatal(&format!(
                "VillFlow V4 couldn't open its data folder:\n{e:#}"
            ));
            return;
        }
    };
    let configs = providers::load_configs(store.dir());

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app)
        }))
        .manage(store.clone())
        .manage(Configs(Mutex::new(configs)))
        .manage(Loaded(Mutex::new(None)))
        .manage(providers::WriterLoaded(Mutex::new(None)))
        .manage(HotkeyError(Mutex::new(None)))
        .manage(Arc::new(idle::Activity::default()))
        .invoke_handler(tauri::generate_handler![
            get_state,
            save_settings,
            hotkeys_pause,
            history,
            delete_entry,
            clear_history,
            copy_text,
            provider_open,
            provider_show,
            test_dictation,
            debug_write,
            provider_hide,
            provider_status,
            provider_inspect,
            customize_selectors,
            reload_selectors,
            reset_selectors,
            open_data_dir,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            overlay::start();

            let settings = store.settings();
            idle::spawn(handle.clone());
            match hotkeys::start(&settings.hotkey, &settings.command_hotkey) {
                Ok(rx) => dictation::spawn(handle.clone(), rx),
                Err(e) => {
                    log::error!("hotkey unavailable: {e}");
                    overlay::error("VillFlow's hotkey isn't working. Open VillFlow for details");
                    *handle.state::<HotkeyError>().0.lock().unwrap() = Some(e.to_string());
                }
            }
            // Keep the login entry pointing at this exe (it may have moved or been reinstalled).
            if settings.launch_at_login {
                if let Err(e) = set_launch_at_login(true) {
                    log::warn!("launch at login: {e}");
                }
            }

            let icon = app.default_window_icon().cloned();
            let menu = tray_menu(&handle)?;
            let mut tray = TrayIconBuilder::with_id("main");
            if let Some(icon) = icon {
                tray = tray.icon(icon);
            }
            tray.tooltip("VillFlow V4")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, e| on_menu(app, e.id().as_ref()))
                .on_tray_icon_event(|tray, e| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = e
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // Warm up the provider page so the first dictation starts instantly.
            if settings.onboarded {
                load_active_provider(&handle);
                idle::check_pages_after_start(handle.clone());
            }
            if !background || !settings.onboarded {
                show_main(&handle);
            }
            if signin_all {
                let list = handle.state::<Configs>().0.lock().unwrap().clone();
                for cfg in &list {
                    if let Err(e) = providers::open_signin(&handle, cfg) {
                        log::error!("sign-in window for {}: {e}", cfg.id);
                    }
                }
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("build VillFlow V4");

    app.run(|_app, event| {
        // Keep running in the tray when the settings window closes.
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            api.prevent_exit();
        }
    });
}
