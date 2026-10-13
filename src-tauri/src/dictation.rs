//! Hotkey → provider dictation → paste → history.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::assist;
use crate::hotkeys::{HotkeyEvent, Kind};
use crate::inject::{self, OutputMode, Target};
use crate::overlay::{self, Overlay};
use crate::providers::{self, ProviderConfig};
use crate::store::{NewEntry, Settings, Store, TriggerMode};
use crate::Configs;

/// Taps shorter than this are treated as accidental and discarded.
const MIN_HOLD: Duration = Duration::from_millis(300);
/// Safety cap on a single recording.
const MAX_RECORDING: Duration = Duration::from_secs(10 * 60);

/// The provider page shows its logged-out state.
#[derive(Debug)]
struct SignedOut(String);

impl std::fmt::Display for SignedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sign in to {} first (VillFlow → Accounts)", self.0)
    }
}

impl std::error::Error for SignedOut {}

pub fn spawn(app: AppHandle, mut rx: UnboundedReceiver<HotkeyEvent>) {
    tauri::async_runtime::spawn(async move {
        let mut pending: Option<Kind> = None;
        loop {
            let kind = match pending.take() {
                Some(k) => k,
                None => match rx.recv().await {
                    Some(HotkeyEvent::Down(k)) => k,
                    Some(_) => continue,
                    None => break,
                },
            };
            log::debug!("hotkey down: {kind:?}");
            if let Err(e) = session(&app, &mut rx, kind).await {
                log::error!("{kind:?} failed: {e:#}");
                overlay::error(short(&e.to_string()));
                notify_ui(&app, "vf://error", Value::String(e.to_string()));
                if e.downcast_ref::<SignedOut>().is_some() {
                    crate::show_main_at(&app, "providers");
                }
            }
            // Presses that arrived while we were busy: if the last thing that
            // happened is a press that's still held, start that next right away
            // instead of silently dropping it. (Toggle mode: any press counts.)
            let mut last = None;
            let mut last_down = None;
            while let Ok(ev) = rx.try_recv() {
                if let HotkeyEvent::Down(k) = ev {
                    last_down = Some(k);
                }
                last = Some(ev);
            }
            pending = match app.state::<Arc<Store>>().settings().trigger {
                TriggerMode::Hold => match last {
                    Some(HotkeyEvent::Down(k)) => Some(k),
                    _ => None,
                },
                TriggerMode::Toggle => last_down,
            };
        }
    });
}

fn short(s: &str) -> String {
    let mut out: String = s.chars().take(80).collect();
    if s.chars().count() > 80 {
        out.push('…');
    }
    out
}

fn notify_ui(app: &AppHandle, event: &str, payload: Value) {
    if app.get_webview_window("main").is_some() {
        let _ = app.emit_to("main", event, payload);
    }
}

fn active_config(app: &AppHandle, id: &str) -> anyhow::Result<ProviderConfig> {
    let configs = app.state::<Configs>();
    let list = configs.0.lock().unwrap();
    list.iter()
        .find(|p| p.id == id)
        .or_else(|| list.first())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no providers configured"))
}

/// Start dictation, retrying while the page is still loading.
async fn start_with_retry(app: &AppHandle, cfg: &ProviderConfig) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut reloaded = false;
    loop {
        let res = providers::call(app, cfg, "start").await;
        let err = match &res {
            Ok(v) if v["ok"].as_bool() == Some(true) => return Ok(()),
            Ok(v) => v["error"].as_str().unwrap_or("unknown").to_string(),
            Err(e) => e.to_string(),
        };
        if err == "signed_out" {
            return Err(SignedOut(cfg.name.clone()).into());
        }
        let retryable = res
            .as_ref()
            .map(|v| v["retry"].as_bool() == Some(true))
            .unwrap_or(true);
        if !retryable || Instant::now() >= deadline {
            anyhow::bail!("{}: {}", cfg.name, humanize(&err));
        }
        // Reload the page if it isn't answering at all (crashed renderer), or halfway
        // through if it answers but stays unusable (stale SPA state, expired session…).
        // ("not ready" just means mid-navigation, e.g. still loading: don't reload for that.)
        let unresponsive = err.contains("did not respond");
        if !reloaded && (unresponsive || Instant::now() + Duration::from_secs(6) >= deadline) {
            reloaded = true;
            log::warn!("{}: reloading page ({err})", cfg.id);
            if let Some(w) = providers::window(app) {
                if let Ok(url) = cfg.url.parse() {
                    let _ = w.navigate(url);
                }
            }
        }
        overlay::set(Overlay::Working(format!("Waiting for {}…", cfg.name)));
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// The provider that does the writing: the dictation provider ("same"), or
/// another one (e.g. dictate with Gemini, write with ChatGPT).
fn writer_config(
    app: &AppHandle,
    settings: &Settings,
    dictation: &ProviderConfig,
) -> ProviderConfig {
    if settings.writer.is_empty() || settings.writer == "same" || settings.writer == dictation.id {
        return dictation.clone();
    }
    let configs = app.state::<Configs>();
    let list = configs.0.lock().unwrap();
    list.iter()
        .find(|p| p.id == settings.writer)
        .cloned()
        .unwrap_or_else(|| dictation.clone())
}

fn humanize(err: &str) -> String {
    match err {
        "no_mic_button" => "couldn't find the dictation button".into(),
        "no_composer" => "couldn't find the message box".into(),
        "loading" => "page still loading".into(),
        other => other.into(),
    }
}

/// Cancel, and keep cancelling until the site is confirmed idle: after a very
/// short tap the site may only start recording *after* our first cancel.
/// With `only_if_current`, stops as soon as a newer session has started.
async fn cancel_until_idle(app: &AppHandle, cfg: &ProviderConfig, only_if_current: Option<u64>) {
    let activity = app.state::<Arc<crate::idle::Activity>>().inner().clone();
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut quiet_rounds = 0;
    while Instant::now() < deadline && quiet_rounds < 3 {
        if only_if_current.is_some_and(|g| !activity.is_current(g)) {
            return;
        }
        match providers::call(app, cfg, "cancel").await {
            Ok(r) if r["was_recording"].as_bool() == Some(true) => quiet_rounds = 0,
            _ => quiet_rounds += 1,
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
    }
}

/// Safety net after every session: never leave the provider's mic running.
/// Skipped if another session has started meanwhile: that recording is wanted.
fn ensure_idle_later(app: &AppHandle, cfg: ProviderConfig, generation: u64) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let activity = app.state::<Arc<crate::idle::Activity>>().inner().clone();
        if !activity.is_current(generation) {
            return;
        }
        if let Ok(s) = providers::call(&app, &cfg, "status").await {
            if !activity.is_current(generation) {
                return;
            }
            if s["recording"].as_bool() == Some(true) {
                log::warn!("{} still recording after session; cancelling", cfg.id);
                cancel_until_idle(&app, &cfg, Some(generation)).await;
            }
        }
    });
}

async fn session(
    app: &AppHandle,
    rx: &mut UnboundedReceiver<HotkeyEvent>,
    kind: Kind,
) -> anyhow::Result<()> {
    let provider = app.state::<Arc<Store>>().settings().provider;
    let activity = app.state::<Arc<crate::idle::Activity>>().inner().clone();
    let generation = activity.begin(app);
    let result = run_session(app, rx, kind).await;
    activity.end();
    if let Ok(cfg) = active_config(app, &provider) {
        if providers::window(app).is_some() {
            ensure_idle_later(app, cfg, generation);
        }
    }
    result
}

/// Test hook: the next session runs the whole real pipeline (mic, provider,
/// writer) but never touches the desktop: no Ctrl+C into the focused app and no
/// pasting. The inner value is a pretend selection for command mode.
pub static DRY_RUN: std::sync::Mutex<Option<Option<String>>> = std::sync::Mutex::new(None);

/// What a press will do, decided when it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Dictate,
    /// Command hotkey with text selected: rewrite it.
    Edit,
    /// Command hotkey, nothing selected: write new text at the cursor.
    Generate,
}

async fn run_session(
    app: &AppHandle,
    rx: &mut UnboundedReceiver<HotkeyEvent>,
    kind: Kind,
) -> anyhow::Result<()> {
    let store = app.state::<Arc<Store>>().inner().clone();
    let settings = store.settings();
    let target = Target::capture();
    let cfg = active_config(app, &settings.provider)?;
    let writer = writer_config(app, &settings, &cfg);
    let pressed_at = Instant::now();

    let dry_run = DRY_RUN.lock().unwrap().take();
    // Command mode: Edit if text is selected in the focused app, else Generate.
    let mut selection = dry_run.clone().flatten();
    let mode = if kind == Kind::Command {
        if writer.ask.is_none() {
            anyhow::bail!(
                "{} can't run commands. Pick Gemini or ChatGPT for commands in VillFlow → Accounts",
                writer.name
            );
        }
        if dry_run.is_none() && !target.is_self() {
            selection =
                tauri::async_runtime::spawn_blocking(move || inject::copy_selection(target))
                    .await?;
        }
        if let Some(sel) = &selection {
            if sel.chars().count() > assist::MAX_SELECTION {
                anyhow::bail!(
                    "That selection is too long to edit (max {} characters)",
                    assist::MAX_SELECTION
                );
            }
        }
        if selection.is_some() {
            Mode::Edit
        } else {
            Mode::Generate
        }
    } else {
        Mode::Dictate
    };
    let label = match mode {
        Mode::Dictate => format!("Listening · {}", cfg.name),
        Mode::Edit => "Selection · say what to do".to_string(),
        Mode::Generate => "Command · ask or say what to write".to_string(),
    };

    if providers::window(app).is_none() {
        overlay::set(Overlay::Working(format!("Loading {}…", cfg.name)));
    }
    providers::ensure(app, &cfg)?;
    // A freshly (re)created page starts at "normal" memory; make sure an idle-trimmed one is too.
    app.state::<Arc<crate::idle::Activity>>().full_speed(app);
    start_with_retry(app, &cfg).await?;

    let started = Instant::now();
    // "Starting…" until the site confirms it's recording, so people don't talk
    // into the gap while it spins up.
    overlay::set(Overlay::Working(format!("Starting {}…", cfg.name)));
    let mut ticker = tokio::time::interval(Duration::from_millis(200));
    let mut recording_at: Option<Instant> = None;
    let mut restarted = false;
    loop {
        tokio::select! {
            ev = rx.recv() => match (settings.trigger, ev) {
                (TriggerMode::Hold, Some(HotkeyEvent::Up(k))) if k == kind => break,
                (TriggerMode::Toggle, Some(HotkeyEvent::Down(k))) if k == kind => break,
                (_, None) => return Ok(()),
                _ => {}
            },
            _ = ticker.tick() => {
                if started.elapsed() > MAX_RECORDING {
                    break;
                }
                if let Ok(t) = providers::call(app, &cfg, "tick").await {
                    if recording_at.is_none() && t["saw_recording"].as_bool() == Some(true) {
                        recording_at = Some(Instant::now());
                        log::info!("{} recording after {} ms", cfg.id, started.elapsed().as_millis());
                    }
                    // The click can land while the SPA is still hydrating and do nothing:
                    // re-click once. (Not for toggle-style sites, where a second
                    // click could stop a recording we failed to detect.)
                    if recording_at.is_none() && !restarted && !cfg.toggle_stop
                        && started.elapsed() > Duration::from_millis(2500)
                    {
                        restarted = true;
                        log::warn!("{} didn't start recording; clicking again", cfg.id);
                        let _ = providers::call(app, &cfg, "start").await;
                    }
                    let live = recording_at
                        .is_some_and(|at| at.elapsed() >= Duration::from_millis(cfg.warmup_ms));
                    // If detection fails on a redesigned page, don't stay on "Starting" forever.
                    if live || started.elapsed() > Duration::from_millis(2500) {
                        overlay::set(Overlay::Listening {
                            label: label.clone(),
                            secs: started.elapsed().as_secs(),
                            preview: t["preview"].as_str().unwrap_or_default().to_string(),
                        });
                    }
                }
            }
        }
    }
    let duration = started.elapsed();
    let saw_recording = recording_at.is_some();

    if settings.trigger == TriggerMode::Hold && duration < MIN_HOLD {
        // Released while the page was still waking up (reload, crash recovery…)
        // vs. a genuine accidental tap.
        if started - pressed_at > Duration::from_millis(1500) {
            overlay::toast(format!("{} is ready now, try again", cfg.name));
        } else {
            overlay::set(Overlay::Hidden);
        }
        cancel_until_idle(app, &cfg, None).await;
        return Ok(());
    }

    overlay::set(Overlay::Working("Transcribing…".into()));
    let stop = providers::call(app, &cfg, "stop").await?;
    log::info!(
        "stop via {} {} (saw recording: {saw_recording})",
        stop["via"],
        stop["button"]
    );

    let deadline = Instant::now() + Duration::from_millis(cfg.timeout_ms + 3000);
    let (spoken, page_error) = loop {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let p = providers::call(app, &cfg, "poll").await?;
        if p["done"].as_bool() == Some(true) {
            log::info!("transcript done ({})", p["reason"]);
            break (
                p["text"].as_str().unwrap_or_default().to_string(),
                p["page_error"].as_str().unwrap_or_default().to_string(),
            );
        }
        if Instant::now() > deadline {
            let _ = providers::call(app, &cfg, "cancel").await;
            anyhow::bail!("{} took too long to transcribe", cfg.name);
        }
    };

    if spoken.trim().is_empty() {
        notify_ui(app, "vf://empty", Value::Null);
        if !page_error.is_empty() {
            log::warn!("{} reported: {page_error}", cfg.id);
            overlay::error(format!("{}: {}", cfg.name, short(&page_error)));
            notify_ui(
                app,
                "vf://error",
                Value::String(format!("{}: {page_error}", cfg.name)),
            );
        } else {
            overlay::toast(if saw_recording {
                "No speech detected".to_string()
            } else {
                format!("{} didn't start recording, check the mic", cfg.name)
            });
        }
        return Ok(());
    }
    let spoken = assist::apply_dictionary(&spoken, &settings.dictionary);

    // Turn what was said into the text to insert.
    let mut warning = None;
    let mut outcome = assist::Outcome::Insert;
    let text = match mode {
        Mode::Dictate if settings.polish && writer.ask.is_some() => {
            overlay::set(Overlay::Working("Polishing…".into()));
            let req = assist::Request::Polish { dictation: &spoken };
            match assist::write(
                app,
                &writer,
                req,
                settings.writing_model,
                &settings.dictionary,
            )
            .await
            {
                Ok(w) => w.text,
                Err(e) => {
                    // Never lose the dictation over a failed polish.
                    log::warn!("polish failed: {e:#}");
                    warning = Some("Couldn't polish, so it's pasted as spoken".to_string());
                    spoken.clone()
                }
            }
        }
        Mode::Dictate => spoken.clone(),
        Mode::Edit | Mode::Generate => {
            overlay::set(Overlay::Working(format!("{} is writing…", writer.name)));
            let req = match &selection {
                Some(sel) => assist::Request::Edit {
                    instruction: &spoken,
                    selection: sel,
                },
                None => assist::Request::Generate {
                    instruction: &spoken,
                },
            };
            let w = assist::write(
                app,
                &writer,
                req,
                settings.writing_model,
                &settings.dictionary,
            )
            .await?;
            log::info!(
                "{:?} via {} ({}, {} ms)",
                w.outcome,
                writer.id,
                w.format,
                w.ms
            );
            outcome = w.outcome;
            w.text
        }
    };
    // Edit + a question about the text: the answer goes after it, not over it.
    let kind_label = match (mode, outcome) {
        (Mode::Dictate, _) => "dictation",
        (Mode::Edit, assist::Outcome::Answer) => "answer",
        (Mode::Edit, assist::Outcome::Append) => "append",
        (Mode::Edit, _) => "edit",
        (Mode::Generate, _) => "generate",
    };

    let self_focused = target.is_self() || dry_run.is_some();
    let app_name = if self_focused {
        String::new()
    } else {
        target.app_name()
    };
    // Saved before delivery, so a failed paste can never lose what you said.
    let note = if mode == Mode::Dictate {
        ""
    } else {
        spoken.as_str()
    };
    let entry = store.record(
        NewEntry {
            text: &text,
            duration_ms: duration.as_millis() as i64,
            provider: if mode == Mode::Dictate {
                &cfg.id
            } else {
                &writer.id
            },
            app: &app_name,
            kind: kind_label,
            note,
        },
        settings.save_history,
    )?;

    if self_focused {
        // Dictating while VillFlow itself is focused (e.g. the "Try it" button):
        // just show the result in the app, don't touch the clipboard.
        overlay::set(Overlay::Hidden);
    } else {
        // Answers go after the selection as a new paragraph; continuations
        // right after it, in the same paragraph.
        let answer_after = matches!(outcome, assist::Outcome::Answer | assist::Outcome::Append);
        let mut out = match outcome {
            assist::Outcome::Answer => format!("\n\n{text}"),
            assist::Outcome::Append if !text.starts_with(char::is_whitespace) => format!(" {text}"),
            _ => text.clone(),
        };
        // A replaced selection keeps its own spacing.
        if settings.add_trailing_space && mode != Mode::Edit && !out.ends_with(char::is_whitespace)
        {
            out.push(' ');
        }
        let output = settings.output;
        let restore = settings.restore_clipboard;
        let (delivered, output) = tauri::async_runtime::spawn_blocking(move || {
            // If the original window can't be brought back (you switched apps
            // while it was transcribing/writing), never paste into whatever is
            // focused now: hand the text over on the clipboard instead.
            let output = if output != OutputMode::Copy && !target.restore() {
                OutputMode::Copy
            } else {
                output
            };
            // Keep the selection; the answer goes right after it.
            if answer_after && output != OutputMode::Copy {
                let _ = inject::collapse_selection_to_end();
            }
            let result = match inject::deliver(&out, output, restore) {
                Ok(()) => Ok(()),
                Err(e) => {
                    // Last resort: leave it on the clipboard for a manual Ctrl+V.
                    let _ = inject::deliver(&out, OutputMode::Copy, false);
                    Err(e)
                }
            };
            (result, output)
        })
        .await?;
        let switched_away = output == OutputMode::Copy && settings.output != OutputMode::Copy;
        overlay::set(match (&delivered, output) {
            (Ok(()), _) if switched_away => Overlay::Toast {
                message: "You switched windows, so it's on your clipboard".into(),
                error: false,
            },
            (Err(e), _) => {
                log::warn!("delivery failed: {e:#}");
                Overlay::Toast {
                    message: "Couldn't paste here, so it's on your clipboard".into(),
                    error: true,
                }
            }
            (Ok(()), _) if warning.is_some() => Overlay::Toast {
                message: warning.clone().unwrap_or_default(),
                error: true,
            },
            (Ok(()), OutputMode::Copy) => Overlay::Toast {
                message: "Copied to clipboard".into(),
                error: false,
            },
            _ => Overlay::Hidden,
        });
    }

    notify_ui(app, "vf://dictation", serde_json::to_value(entry)?);
    Ok(())
}
