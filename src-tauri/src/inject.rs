//! Deliver text to the focused app (ported from V3) + foreground-window helpers.

use std::thread;
use std::time::{Duration, Instant};

use crate::hotkeys;
use arboard::{Clipboard, SetExtWindows};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_C, VK_CONTROL, VK_LWIN, VK_RETURN, VK_RWIN, VK_SHIFT, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// Clipboard + Ctrl+V (fast, works almost everywhere).
    #[default]
    Paste,
    /// Synthesized unicode keystrokes (for apps that block paste).
    Type,
    /// Only put the text on the clipboard.
    Copy,
}

/// A foreground window captured when dictation started, so we can paste back
/// into it even if something stole focus meanwhile.
#[derive(Debug, Clone, Copy)]
pub struct Target(isize);

impl Target {
    pub fn capture() -> Self {
        Target(unsafe { GetForegroundWindow() }.0 as isize)
    }

    fn hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }

    pub fn is_self(self) -> bool {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(self.hwnd(), Some(&mut pid)) };
        pid != 0 && pid == unsafe { GetCurrentProcessId() }
    }

    /// Executable stem of the window's process, e.g. "chrome", "Code".
    pub fn app_name(self) -> String {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(self.hwnd(), Some(&mut pid));
            if pid == 0 {
                return String::new();
            }
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return String::new();
            };
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                h,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(h);
            if !ok {
                return String::new();
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            std::path::Path::new(&path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        }
    }

    /// Bring the window back to the front if focus moved away. Returns whether
    /// it's the foreground window now: if not (closed, or Windows refused the
    /// switch), the caller must NOT type/paste, or the text lands in some other app.
    pub fn restore(self) -> bool {
        unsafe {
            let hwnd = self.hwnd();
            if hwnd.0.is_null() || !IsWindow(Some(hwnd)).as_bool() {
                return false;
            }
            if GetForegroundWindow() == hwnd {
                return true;
            }
            let _ = SetForegroundWindow(hwnd);
            thread::sleep(Duration::from_millis(40));
            let ok = GetForegroundWindow() == hwnd;
            if !ok {
                log::info!("focus moved to another app; not pasting there");
            }
            ok
        }
    }
}

/// Terminal windows (by window class), where Ctrl+C means "interrupt", not
/// "copy". Matched by class rather than process: plenty of ordinary windows
/// are hosted by powershell.exe or cmd.exe.
const TERMINAL_CLASSES: [&str; 6] = [
    "CASCADIA_HOSTING_WINDOW_CLASS", // Windows Terminal
    "ConsoleWindowClass",            // classic console (cmd, PowerShell)
    "PseudoConsoleWindow",
    "org.wezfurlong.wezterm",
    "mintty",
    "Alacritty",
];

impl Target {
    fn class_name(self) -> String {
        use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
        let mut buf = [0u16; 256];
        let len = unsafe { GetClassNameW(self.hwnd(), &mut buf) };
        String::from_utf16_lossy(&buf[..len.max(0) as usize])
    }

    pub fn is_terminal(self) -> bool {
        let class = self.class_name();
        TERMINAL_CLASSES
            .iter()
            .any(|c| class.eq_ignore_ascii_case(c))
    }
}

/// The text currently selected in the focused app, if any (via a quick Ctrl+C;
/// the clipboard is put back afterwards). `None` in terminals, where Ctrl+C
/// would interrupt a running program.
pub fn copy_selection(target: Target) -> Option<String> {
    use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
    if target.is_terminal() {
        log::info!("terminal focused: command mode will generate, not edit");
        return None;
    }
    let snapshot = ClipboardSnapshot::take();
    let before = unsafe { GetClipboardSequenceNumber() };
    // The user is holding the command hotkey: lift those keys so the app sees
    // a plain Ctrl+C, then put back whatever is still held.
    let lifted = hotkeys::held_modifiers();
    let _ = send(
        &lifted
            .iter()
            .map(|&vk| key_input(vk, true))
            .collect::<Vec<_>>(),
    );
    let _ = send(&[
        key_input(VK_CONTROL, false),
        key_input(VK_C, false),
        key_input(VK_C, true),
        key_input(VK_CONTROL, true),
    ]);
    let deadline = Instant::now() + Duration::from_millis(350);
    let mut changed = false;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(15));
        if unsafe { GetClipboardSequenceNumber() } != before {
            changed = true;
            thread::sleep(Duration::from_millis(30)); // let the app finish writing
            break;
        }
    }
    let still = hotkeys::held_modifiers();
    let _ = send(
        &lifted
            .iter()
            .filter(|vk| still.contains(vk))
            .map(|&vk| key_input(vk, false))
            .collect::<Vec<_>>(),
    );
    if !changed {
        return None;
    }
    let text = Clipboard::new().and_then(|mut c| c.get_text()).ok();
    if let Some(s) = snapshot {
        s.restore();
    }
    text.filter(|t| !t.trim().is_empty())
}

/// Collapse the current selection to its end (Right arrow does that in
/// practically every editor), so a following paste lands after it instead of
/// replacing it.
pub fn collapse_selection_to_end() -> anyhow::Result<()> {
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_RIGHT;
    let lifted = hotkeys::held_modifiers();
    send(
        &lifted
            .iter()
            .map(|&vk| key_input(vk, true))
            .collect::<Vec<_>>(),
    )?;
    send(&[key_input(VK_RIGHT, false), key_input(VK_RIGHT, true)])?;
    thread::sleep(Duration::from_millis(30));
    Ok(())
}

/// Deliver `text` to the focused app.
pub fn deliver(text: &str, mode: OutputMode, restore_clipboard: bool) -> anyhow::Result<()> {
    if mode == OutputMode::Copy {
        return Clipboard::new()
            .and_then(|mut c| c.set_text(text.to_string()))
            .map_err(Into::into);
    }
    // Give a just-released push-to-talk combo a moment to come up.
    let deadline = Instant::now() + Duration::from_millis(150);
    while !hotkeys::held_modifiers().is_empty() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(15));
    }
    // Anything still held (e.g. already holding the hotkey for the next
    // dictation) would turn Ctrl+V into Ctrl+Shift+V, or typed text into
    // shortcuts. Lift those keys just for the delivery, then put back the ones
    // that are still really held, so the user's combo keeps working.
    let lifted = hotkeys::held_modifiers();
    let _ = send(
        &lifted
            .iter()
            .map(|&vk| key_input(vk, true))
            .collect::<Vec<_>>(),
    );
    let result = match mode {
        OutputMode::Type => type_text(text),
        _ => paste(text, restore_clipboard),
    };
    let still_held = hotkeys::held_modifiers();
    let restore: Vec<INPUT> = lifted
        .iter()
        .filter(|vk| still_held.contains(vk))
        .map(|&vk| key_input(vk, false))
        .collect();
    let _ = send(&restore);
    clear_stuck_modifiers(&still_held);
    result
}

/// A modifier Windows thinks is down but nobody is physically holding (some
/// apps swallow key-ups) would make every later click a Ctrl/Shift-click.
fn clear_stuck_modifiers(really_held: &[VIRTUAL_KEY]) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_RCONTROL, VK_RMENU, VK_RSHIFT,
    };
    let stuck: Vec<INPUT> = [
        VK_LCONTROL,
        VK_RCONTROL,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_LMENU,
        VK_RMENU,
        VK_LWIN,
        VK_RWIN,
    ]
    .into_iter()
    .filter(|vk| !really_held.contains(vk))
    .filter(|vk| unsafe { GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0 })
    .map(|vk| key_input(vk, true))
    .collect();
    let _ = send(&stuck);
}

fn paste(text: &str, restore_clipboard: bool) -> anyhow::Result<()> {
    // Snapshot *every* format (text, images, copied files, rich text…), not just
    // text, so "keep my clipboard" never destroys a copied image or file.
    let previous = if restore_clipboard {
        ClipboardSnapshot::take()
    } else {
        None
    };
    // Kept out of Win+V clipboard history and cloud sync: it's only a vehicle.
    Clipboard::new()?
        .set()
        .exclude_from_monitoring()
        .text(text.to_string())?;
    thread::sleep(Duration::from_millis(15));
    let inputs = [
        key_input(VK_CONTROL, false),
        key_input(VK_V, false),
        key_input(VK_V, true),
        key_input(VK_CONTROL, true),
    ];
    send(&inputs)?;
    if let Some(previous) = previous {
        // Electron/Office read the clipboard lazily; restoring too early pastes the old value.
        thread::sleep(Duration::from_millis(250));
        previous.restore();
    }
    Ok(())
}

/// Raw copy of every memory-backed clipboard format.
struct ClipboardSnapshot(Vec<(u32, Vec<u8>)>);

impl ClipboardSnapshot {
    /// GDI-handle formats can't be copied as bytes; Windows re-synthesizes them
    /// from their memory-backed siblings (CF_DIB → CF_BITMAP etc.).
    const HANDLE_FORMATS: [u32; 8] = [2, 3, 9, 14, 0x80, 0x82, 0x83, 0x8E];

    fn take() -> Option<Self> {
        use clipboard_win::raw;
        let _guard = open_clipboard()?;
        let formats: Vec<u32> = raw::EnumFormats::new()
            .filter(|f| !Self::HANDLE_FORMATS.contains(f))
            .collect();
        let mut items = Vec::new();
        for f in formats {
            let mut buf = Vec::new();
            if raw::get_vec(f, &mut buf).is_ok() && !buf.is_empty() {
                items.push((f, buf));
            }
        }
        Some(Self(items))
    }

    fn restore(self) {
        use clipboard_win::raw;
        let Some(_guard) = open_clipboard() else {
            log::warn!("clipboard busy; previous contents not restored");
            return;
        };
        let _ = raw::empty();
        for (f, data) in &self.0 {
            let _ = raw::set_without_clear(*f, data);
        }
    }
}

/// Another app may hold the clipboard for a moment; retry briefly.
fn open_clipboard() -> Option<ClipboardGuard> {
    for _ in 0..20 {
        if clipboard_win::raw::open().is_ok() {
            return Some(ClipboardGuard);
        }
        thread::sleep(Duration::from_millis(10));
    }
    None
}

struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        let _ = clipboard_win::raw::close();
    }
}

fn type_text(text: &str) -> anyhow::Result<()> {
    let text = text.replace("\r\n", "\n");
    for line_or_break in text.split_inclusive('\n') {
        let (line, newline) = match line_or_break.strip_suffix('\n') {
            Some(l) => (l, true),
            None => (line_or_break, false),
        };
        for unit in line.encode_utf16() {
            send(&[unicode_input(unit, false), unicode_input(unit, true)])?;
            thread::sleep(Duration::from_millis(1));
        }
        // A raw '\n' is ignored by most apps, and Enter alone sends the message in
        // chat apps; Shift+Enter is a line break nearly everywhere.
        if newline {
            send(&[
                key_input(VK_SHIFT, false),
                key_input(VK_RETURN, false),
                key_input(VK_RETURN, true),
                key_input(VK_SHIFT, true),
            ])?;
        }
    }
    Ok(())
}

fn send(inputs: &[INPUT]) -> anyhow::Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    if unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) } == 0 {
        anyhow::bail!(
            "Windows blocked the keystrokes (is the target app running as administrator?)"
        );
    }
    Ok(())
}

fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                time: 0,
                dwExtraInfo: hotkeys::VF_MARK,
            },
        },
    }
}

fn unicode_input(unit: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: if up {
                    KEYEVENTF_KEYUP | KEYEVENTF_UNICODE
                } else {
                    KEYEVENTF_UNICODE
                },
                time: 0,
                dwExtraInfo: hotkeys::VF_MARK,
            },
        },
    }
}
