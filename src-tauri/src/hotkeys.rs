//! Global push-to-talk hotkey via `WH_KEYBOARD_LL`.
//!
//! Push-to-talk needs key-up, which `RegisterHotKey` can't give us. Only the
//! combo's MAIN key is ever swallowed; modifier downs/ups always pass through
//! so modifier state can never get stuck for other apps.
//!
//! Keys VillFlow itself synthesizes (Ctrl+V, typing, the Alt/Win mask) carry
//! [`VF_MARK`] in `dwExtraInfo` and are ignored here. Every other key counts,
//! including injected ones (remote desktop, on-screen keyboards, AutoHotkey).
//! The hook also tracks which modifiers are *really* held, so pasting can lift
//! them briefly and put them back without breaking a combo the user is holding.

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;

use tokio::sync::mpsc;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL,
    VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage, HC_ACTION,
    KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// `dwExtraInfo` tag on every key VillFlow sends ("VFLW").
pub const VF_MARK: usize = 0x5646_4C57;

/// Which hotkey: plain dictation, or command mode (edit selection / generate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dictate,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Down(Kind),
    Up(Kind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCombo {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
    pub key_vk: u16,
}

impl KeyCombo {
    /// Parse strings like `Ctrl+Shift+Space`, case-insensitive. Requires at least
    /// one modifier so a bad setting can't swallow every press of a letter.
    pub fn parse(s: &str) -> Option<Self> {
        let (mut ctrl, mut shift, mut alt, mut win) = (false, false, false, false);
        let mut key_vk: Option<u16> = None;
        for part in s.split('+') {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }
            let lower = p.to_ascii_lowercase();
            match lower.as_str() {
                "ctrl" | "control" => ctrl = true,
                "shift" => shift = true,
                "alt" | "menu" => alt = true,
                "win" | "super" | "meta" | "cmd" => win = true,
                _ => {
                    let vk = if p.chars().count() == 1 {
                        let c = p.chars().next()?.to_ascii_uppercase();
                        match c {
                            'A'..='Z' | '0'..='9' => c as u16,
                            '`' => 0xC0,
                            _ => return None,
                        }
                    } else {
                        match lower.as_str() {
                            "space" => 0x20,
                            "tab" => 0x09,
                            "enter" | "return" => 0x0D,
                            "capslock" => 0x14,
                            "backquote" => 0xC0,
                            f if f.starts_with('f') => {
                                let n: u16 = f[1..].parse().ok()?;
                                if !(1..=24).contains(&n) {
                                    return None;
                                }
                                0x70 + n - 1
                            }
                            _ => return None,
                        }
                    };
                    if key_vk.replace(vk).is_some() {
                        return None; // two main keys
                    }
                }
            }
        }
        if !(ctrl || shift || alt || win) {
            return None;
        }
        Some(Self {
            ctrl,
            shift,
            alt,
            win,
            key_vk: key_vk?,
        })
    }

    fn involves(&self, vk: u16) -> bool {
        self.key_vk == vk
            || (self.ctrl && is_ctrl_vk(vk))
            || (self.shift && is_shift_vk(vk))
            || (self.alt && is_alt_vk(vk))
            || (self.win && is_win_vk(vk))
    }
}

struct Binding {
    kind: Kind,
    combo: KeyCombo,
    engaged: bool,
    /// The main key went down as part of the combo and hasn't come up yet. Its
    /// repeats/release are swallowed even if a modifier was let go first, so a
    /// held Space doesn't start typing spaces into the app.
    main_held: bool,
}

struct HookState {
    bindings: Vec<Binding>,
    tx: mpsc::UnboundedSender<HotkeyEvent>,
}

fn bindings(dictate: &str, command: &str) -> anyhow::Result<Vec<Binding>> {
    let mut out = vec![Binding {
        kind: Kind::Dictate,
        combo: KeyCombo::parse(dictate).ok_or_else(|| {
            anyhow::anyhow!(
                "\"{dictate}\" isn't a valid hotkey: use Ctrl, Shift, Alt or Win plus a key"
            )
        })?,
        engaged: false,
        main_held: false,
    }];
    // Command mode is optional: an empty setting turns it off.
    if !command.trim().is_empty() {
        let combo = KeyCombo::parse(command)
            .ok_or_else(|| anyhow::anyhow!("\"{command}\" isn't a valid command hotkey: use Ctrl, Shift, Alt or Win plus a key"))?;
        if combo == out[0].combo {
            anyhow::bail!("The dictation and command hotkeys must be different");
        }
        out.push(Binding {
            kind: Kind::Command,
            combo,
            engaged: false,
            main_held: false,
        });
    }
    Ok(out)
}

static STATE: Mutex<Option<HookState>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
/// While the settings window records a new hotkey, presses don't trigger
/// anything (and reach the window). Expires on its own, so a closed window
/// can't leave the hotkeys off.
static PAUSED_UNTIL: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

/// Pause the hotkeys for up to `ms` milliseconds (0 = resume now).
pub fn pause(ms: u64) {
    let until = if ms == 0 { 0 } else { now_ms() + ms };
    PAUSED_UNTIL.store(until, Ordering::SeqCst);
}

fn paused() -> bool {
    now_ms() < PAUSED_UNTIL.load(Ordering::SeqCst)
}

/// Modifiers really held right now (not counting VillFlow's own keys), one bit
/// per left/right key, in the order of [`MODIFIERS`].
static PHYSICAL: AtomicU16 = AtomicU16::new(0);
const MODIFIERS: [VIRTUAL_KEY; 8] = [
    VK_LCONTROL,
    VK_RCONTROL,
    VK_LSHIFT,
    VK_RSHIFT,
    VK_LMENU,
    VK_RMENU,
    VK_LWIN,
    VK_RWIN,
];

/// Modifier keys the user is physically holding right now.
pub fn held_modifiers() -> Vec<VIRTUAL_KEY> {
    let bits = PHYSICAL.load(Ordering::SeqCst);
    MODIFIERS
        .iter()
        .enumerate()
        .filter(|(i, _)| bits & (1 << i) != 0)
        .map(|(_, vk)| *vk)
        .collect()
}

fn track_physical(vk: u16, down: bool) {
    // Generic VK_CONTROL/VK_SHIFT/VK_MENU can show up from some injectors; map to left.
    let vk = match vk {
        v if v == VK_CONTROL.0 => VK_LCONTROL.0,
        v if v == VK_SHIFT.0 => VK_LSHIFT.0,
        v if v == VK_MENU.0 => VK_LMENU.0,
        v => v,
    };
    if let Some(i) = MODIFIERS.iter().position(|m| m.0 == vk) {
        if down {
            PHYSICAL.fetch_or(1 << i, Ordering::SeqCst);
        } else {
            PHYSICAL.fetch_and(!(1 << i), Ordering::SeqCst);
        }
    }
}

fn physical_mods() -> (bool, bool, bool, bool) {
    let b = PHYSICAL.load(Ordering::SeqCst);
    (
        b & 0b11 != 0,
        b & 0b1100 != 0,
        b & 0b11_0000 != 0,
        b & 0b1100_0000 != 0,
    )
}

/// Install the hook (once per process) and return the event stream.
pub fn start(dictate: &str, command: &str) -> anyhow::Result<mpsc::UnboundedReceiver<HotkeyEvent>> {
    let bindings = bindings(dictate, command)?;
    let (tx, rx) = mpsc::unbounded_channel();
    *STATE.lock().unwrap() = Some(HookState { bindings, tx });

    if RUNNING.swap(true, Ordering::SeqCst) {
        return Ok(rx);
    }
    let (ok_tx, ok_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    thread::Builder::new()
        .name("vf-hotkeys".into())
        .spawn(move || unsafe {
            match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) {
                Ok(_hook) => {
                    let _ = ok_tx.send(Ok(()));
                    // LL hooks need a message pump on the installing thread.
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                Err(e) => {
                    let _ = ok_tx.send(Err(e.to_string()));
                }
            }
            RUNNING.store(false, Ordering::SeqCst);
        })?;
    match ok_rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Err(e)) => anyhow::bail!("keyboard hook install failed: {e}"),
        _ => Ok(rx),
    }
}

/// Swap the combos at runtime (settings change).
pub fn set_combos(dictate: &str, command: &str) -> anyhow::Result<()> {
    let parsed = bindings(dictate, command)?;
    if let Some(st) = STATE.lock().unwrap().as_mut() {
        st.bindings = parsed;
    }
    Ok(())
}

/// Check a pair of hotkey settings without applying them.
pub fn validate(dictate: &str, command: &str) -> anyhow::Result<()> {
    bindings(dictate, command).map(|_| ())
}

/// Feed a synthetic press/release (the "Try it" button and tests).
pub fn simulate(ev: HotkeyEvent) {
    if let Some(st) = STATE.lock().unwrap().as_ref() {
        let _ = st.tx.send(ev);
    }
}

fn is_ctrl_vk(vk: u16) -> bool {
    vk == VK_CONTROL.0 || vk == VK_LCONTROL.0 || vk == VK_RCONTROL.0
}
fn is_shift_vk(vk: u16) -> bool {
    vk == VK_SHIFT.0 || vk == VK_LSHIFT.0 || vk == VK_RSHIFT.0
}
fn is_alt_vk(vk: u16) -> bool {
    vk == VK_MENU.0 || vk == VK_LMENU.0 || vk == VK_RMENU.0
}
fn is_win_vk(vk: u16) -> bool {
    vk == VK_LWIN.0 || vk == VK_RWIN.0
}
fn is_modifier_vk(vk: u16) -> bool {
    is_ctrl_vk(vk) || is_shift_vk(vk) || is_alt_vk(vk) || is_win_vk(vk)
}

/// Alt/Win pressed and released "alone" (we swallowed the main key) would open
/// the app's menu bar / the Start menu. A tap of an unassigned key in between
/// masks that, the same trick AutoHotkey uses.
unsafe fn send_mask_key() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    };
    let key = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0xE8), // unassigned
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                dwExtraInfo: VF_MARK,
                ..Default::default()
            },
        },
    };
    SendInput(
        &[key(false), key(true)],
        std::mem::size_of::<INPUT>() as i32,
    );
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code as u32 != HC_ACTION {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    // Our own synthesized keys (paste, typing, masking) are invisible to us.
    if kb.dwExtraInfo == VF_MARK {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let vk = kb.vkCode as u16;
    let msg = wparam.0 as u32;
    let is_down = matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN);
    let is_up = matches!(msg, WM_KEYUP | WM_SYSKEYUP);
    if is_modifier_vk(vk) && (is_down || is_up) {
        track_physical(vk, is_down);
    }

    let mut swallow = false;
    if let Ok(mut guard) = STATE.lock() {
        if let Some(st) = guard.as_mut() {
            let tx = st.tx.clone();
            if is_down && !is_modifier_vk(vk) && !paused() {
                let (ctrl, shift, alt, win) = physical_mods();
                for b in st.bindings.iter_mut().filter(|b| b.combo.key_vk == vk) {
                    if b.main_held {
                        swallow = true; // key repeat (even if a modifier was released)
                    } else if b.combo.ctrl == ctrl
                        && b.combo.shift == shift
                        && b.combo.alt == alt
                        && b.combo.win == win
                    {
                        b.engaged = true;
                        b.main_held = true;
                        swallow = true;
                        if b.combo.alt || b.combo.win {
                            send_mask_key();
                        }
                        let _ = tx.send(HotkeyEvent::Down(b.kind));
                    }
                }
            } else if is_up {
                for b in st.bindings.iter_mut() {
                    if b.engaged && b.combo.involves(vk) {
                        b.engaged = false;
                        let _ = tx.send(HotkeyEvent::Up(b.kind));
                    }
                    // Swallow only the main key's release; eating a modifier key-up
                    // would leave it logically stuck for every other app.
                    if vk == b.combo.key_vk && b.main_held {
                        b.main_held = false;
                        swallow = true;
                    }
                }
            }
        }
    }
    if swallow {
        LRESULT(1)
    } else {
        CallNextHookEx(None, code, wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_expires_and_resumes() {
        pause(60_000);
        assert!(paused());
        pause(0);
        assert!(!paused());
        pause(1);
        std::thread::sleep(std::time::Duration::from_millis(40));
        assert!(!paused(), "a pause ends on its own");
    }

    #[test]
    fn parses_common_combos() {
        let c = KeyCombo::parse("Ctrl+Shift+Space").unwrap();
        assert!(c.ctrl && c.shift && !c.alt && !c.win);
        assert_eq!(c.key_vk, 0x20);
        assert_eq!(KeyCombo::parse("alt+f9").unwrap().key_vk, 0x78);
        assert_eq!(KeyCombo::parse("ctrl+shift+z").unwrap().key_vk, b'Z' as u16);
        assert_eq!(KeyCombo::parse("Ctrl+`").unwrap().key_vk, 0xC0);
    }

    #[test]
    fn rejects_bad_combos() {
        assert!(KeyCombo::parse("Z").is_none());
        assert!(KeyCombo::parse("Space").is_none());
        assert!(KeyCombo::parse("Ctrl+").is_none());
        assert!(KeyCombo::parse("Ctrl+A+B").is_none());
        assert!(KeyCombo::parse("Ctrl+F25").is_none());
    }

    #[test]
    fn tracks_physical_modifiers() {
        track_physical(VK_LCONTROL.0, true);
        track_physical(VK_RSHIFT.0, true);
        assert_eq!(physical_mods(), (true, true, false, false));
        assert_eq!(held_modifiers(), vec![VK_LCONTROL, VK_RSHIFT]);
        track_physical(VK_LCONTROL.0, false);
        track_physical(VK_RSHIFT.0, false);
        assert!(held_modifiers().is_empty());
    }
}
