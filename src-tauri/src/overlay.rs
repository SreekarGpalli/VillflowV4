//! Tiny native Win32 "flow bar" pill, bottom-center of the active monitor.
//!
//! `WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`: never takes focus,
//! never shows in the taskbar. Drawn in software into a per-pixel-alpha layered
//! window (anti-aliased edges, no GPU, ~nothing in RAM/CPU). Sized per monitor
//! DPI, so it looks the same at 100% and 250% scaling.

use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GdiFlush,
    GetDC, GetMonitorInfoW, GetTextExtentPoint32W, MonitorFromWindow, ReleaseDC, SelectObject,
    SetBkMode, SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    BLENDFUNCTION, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH,
    DIB_RGB_COLORS, DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, FW_NORMAL, FW_SEMIBOLD,
    HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow, GetMessageW, KillTimer,
    PostMessageW, RegisterClassW, SetTimer, SetWindowPos, ShowWindow, TranslateMessage,
    UpdateLayeredWindow, HWND_TOPMOST, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    SW_HIDE, ULW_ALPHA, WM_TIMER, WM_USER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const WM_REFRESH: u32 = WM_USER + 41;
const TIMER_TOAST: usize = 1;
const TIMER_SPIN: usize = 2;
const SPIN_MS: u32 = 33;
/// Layout in 96-DPI pixels; multiplied by the monitor's scale.
const HEIGHT: f32 = 40.0;
const FONT: f32 = 14.5;
const PAD_LEFT: f32 = 40.0;
const PAD_RIGHT: f32 = 20.0;
const MIN_W: f32 = 172.0;
const MAX_W: f32 = 600.0;
const BOTTOM_GAP: f32 = 28.0;
/// Live preview shows the last N characters (the newest words).
const PREVIEW_CHARS: usize = 60;

/// Colors (r, g, b), from the settings window's dark palette.
const BG: [u8; 3] = [0x1b, 0x1e, 0x1d];
const EDGE: [u8; 3] = [0x40, 0x46, 0x44];
const TEXT: [u8; 3] = [0xf1, 0xf3, 0xf2];
const RED: [u8; 3] = [0xff, 0x5f, 0x52];
const AMBER: [u8; 3] = [0xf2, 0xb1, 0x4a];
const GREEN: [u8; 3] = [0x6f, 0xca, 0x9b];
const OPACITY: f32 = 0.97;

#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    Hidden,
    /// Recording. `label` e.g. "Listening · Gemini" or "Selection · say what to do";
    /// `preview` = live text when the provider streams it.
    Listening {
        label: String,
        secs: u64,
        preview: String,
    },
    /// Waiting for the provider (loading, starting, transcribing).
    Working(String),
    Toast {
        message: String,
        error: bool,
    },
}

struct Shared {
    state: Overlay,
    hwnd: isize,
    toast_until: Option<Instant>,
    /// Layout computed in `refresh` for the monitor the pill is on.
    scale: f32,
    rect: RECT,
    /// Spinner animation frame (while working).
    frame: u32,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    state: Overlay::Hidden,
    hwnd: 0,
    toast_until: None,
    scale: 1.0,
    rect: RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    },
    frame: 0,
});

pub fn start() {
    thread::Builder::new()
        .name("vf-overlay".into())
        .spawn(|| unsafe {
            if let Err(e) = run() {
                log::error!("overlay failed: {e}");
            }
        })
        .expect("spawn overlay thread");
}

pub fn set(state: Overlay) {
    let hwnd = {
        let mut s = SHARED.lock().unwrap();
        if s.state == state {
            return;
        }
        s.toast_until = match &state {
            // Errors stay up longer so there's time to read them.
            Overlay::Toast { error: true, .. } => {
                Some(Instant::now() + Duration::from_millis(4000))
            }
            Overlay::Toast { .. } => Some(Instant::now() + Duration::from_millis(2200)),
            _ => None,
        };
        s.state = state;
        s.hwnd
    };
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_REFRESH, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn toast(message: impl Into<String>) {
    set(Overlay::Toast {
        message: message.into(),
        error: false,
    });
}

pub fn error(message: impl Into<String>) {
    set(Overlay::Toast {
        message: message.into(),
        error: true,
    });
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn run() -> anyhow::Result<()> {
    let class = wide("VillFlowV4Overlay");
    let hinst = GetModuleHandleW(None)?;
    RegisterClassW(&WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinst.into(),
        lpszClassName: PCWSTR(class.as_ptr()),
        ..Default::default()
    });
    let title = wide("VillFlow V4 overlay");
    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
        PCWSTR(class.as_ptr()),
        PCWSTR(title.as_ptr()),
        WS_POPUP,
        0,
        0,
        200,
        HEIGHT as i32,
        None,
        None,
        Some(hinst.into()),
        None,
    )?;
    SHARED.lock().unwrap().hwnd = hwnd.0 as isize;

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    Ok(())
}

fn label_of(state: &Overlay) -> String {
    match state {
        Overlay::Hidden => String::new(),
        Overlay::Listening {
            label,
            secs,
            preview,
        } if preview.is_empty() => {
            format!("{label}   {}:{:02}", secs / 60, secs % 60)
        }
        Overlay::Listening { preview, .. } => {
            let chars: Vec<char> = preview.chars().filter(|c| *c != '\n').collect();
            if chars.len() > PREVIEW_CHARS {
                let tail: String = chars[chars.len() - PREVIEW_CHARS..].iter().collect();
                format!("…{}", tail.trim_start())
            } else {
                chars.into_iter().collect()
            }
        }
        Overlay::Working(s) => s.clone(),
        Overlay::Toast { message, .. } => message.clone(),
    }
}

fn is_preview(state: &Overlay) -> bool {
    matches!(state, Overlay::Listening { preview, .. } if !preview.is_empty())
}

unsafe fn font(scale: f32, preview: bool) -> HFONT {
    let face = wide("Segoe UI Variable Text");
    let weight = if preview { FW_NORMAL.0 } else { FW_SEMIBOLD.0 } as i32;
    CreateFontW(
        -((FONT * scale).round() as i32),
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        DEFAULT_PITCH.0 as u32,
        PCWSTR(face.as_ptr()),
    )
}

/// Width of `text` in pixels with the pill's font at `scale`.
unsafe fn measure(hwnd: HWND, text: &str, scale: f32, preview: bool) -> i32 {
    let hdc = GetDC(Some(hwnd));
    let f = font(scale, preview);
    let old = SelectObject(hdc, f.into());
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &units, &mut size);
    SelectObject(hdc, old);
    let _ = DeleteObject(f.into());
    ReleaseDC(Some(hwnd), hdc);
    size.cx
}

unsafe fn refresh(hwnd: HWND) {
    let (state, until) = {
        let s = SHARED.lock().unwrap();
        (s.state.clone(), s.toast_until)
    };
    if state == Overlay::Hidden {
        let _ = KillTimer(Some(hwnd), TIMER_SPIN);
        let _ = ShowWindow(hwnd, SW_HIDE);
        return;
    }
    // Bottom-center of the monitor holding the focused window, at its DPI.
    let mon = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = GetMonitorInfoW(mon, &mut mi);
    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
    let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    let scale = dpi_x.max(96) as f32 / 96.0;

    let wa = mi.rcWork;
    let px = |v: f32| (v * scale).round() as i32;
    // Measure the timer as "0:00" so the pill doesn't twitch as digits change.
    let mut label = label_of(&state);
    if matches!(&state, Overlay::Listening { preview, .. } if preview.is_empty()) {
        label = label
            .chars()
            .map(|c| if c.is_ascii_digit() { '0' } else { c })
            .collect();
    }
    let text_w = measure(hwnd, &label, scale, is_preview(&state));
    let max_w = px(MAX_W).min(wa.right - wa.left - px(40.0));
    let w = (text_w + px(PAD_LEFT) + px(PAD_RIGHT)).clamp(px(MIN_W).min(max_w), max_w);
    let h = px(HEIGHT);
    let x = wa.left + (wa.right - wa.left - w) / 2;
    let y = wa.bottom - h - px(BOTTOM_GAP);
    {
        let mut s = SHARED.lock().unwrap();
        s.scale = scale;
        s.rect = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
    }
    render(hwnd);
    let _ = SetWindowPos(
        hwnd,
        Some(HWND_TOPMOST),
        0,
        0,
        0,
        0,
        SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
    );
    if matches!(state, Overlay::Working(_)) {
        SetTimer(Some(hwnd), TIMER_SPIN, SPIN_MS, None);
    } else {
        let _ = KillTimer(Some(hwnd), TIMER_SPIN);
    }
    if let Some(until) = until {
        let ms = until.saturating_duration_since(Instant::now()).as_millis() as u32;
        SetTimer(Some(hwnd), TIMER_TOAST, ms.max(1), None);
    }
}

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

/// Blend `color` (r, g, b) over the BGRA pixel at byte offset `i` with coverage `c`.
fn blend(buf: &mut [u8], i: usize, color: [u8; 3], c: f32) {
    if c <= 0.0 {
        return;
    }
    for (k, ch) in [2usize, 1, 0].into_iter().enumerate() {
        let old = buf[i + k] as f32;
        buf[i + k] = (old + (color[ch] as f32 - old) * c).round() as u8;
    }
}

/// Signed distance from (x, y) to the edge of a horizontal pill of size w x h
/// (negative inside).
fn pill_distance(x: f32, y: f32, w: f32, h: f32) -> f32 {
    let r = h / 2.0;
    let cx = x.clamp(r, w - r);
    ((x - cx).powi(2) + (y - r).powi(2)).sqrt() - r
}

/// Coverage of the status mark at offset (dx, dy) from its center: a dot
/// (recording, done, error) or a spinning arc (working).
fn mark_at(state: &Overlay, scale: f32, frame: u32, dx: f32, dy: f32) -> Option<([u8; 3], f32)> {
    let d = (dx * dx + dy * dy).sqrt();
    match state {
        Overlay::Working(_) => {
            let (radius, half) = (6.0 * scale, 1.1 * scale);
            let ring = clamp01(half + 0.5 - (d - radius).abs());
            if ring <= 0.0 {
                return None;
            }
            let sweep = std::f32::consts::PI * 1.4;
            let a = (dy.atan2(dx) - frame as f32 * 0.21).rem_euclid(std::f32::consts::TAU);
            let along = clamp01(a * radius + 0.5) * clamp01((sweep - a) * radius + 0.5);
            Some(if along > 0.0 {
                (AMBER, ring * along)
            } else {
                (EDGE, ring)
            })
        }
        Overlay::Listening { .. } | Overlay::Toast { error: true, .. } => {
            Some((RED, clamp01(5.0 * scale - d + 0.5)))
        }
        _ => Some((GREEN, clamp01(5.0 * scale - d + 0.5))),
    }
}

/// Draw the pill into a 32-bit premultiplied bitmap and hand it to the window.
unsafe fn render(hwnd: HWND) {
    let (state, scale, rect, frame) = {
        let s = SHARED.lock().unwrap();
        (s.state.clone(), s.scale, s.rect, s.frame)
    };
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    if state == Overlay::Hidden || w <= 0 || h <= 0 {
        return;
    }
    let screen = GetDC(None);
    let mem = CreateCompatibleDC(Some(screen));
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down rows
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let dib = match CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        Ok(d) if !bits.is_null() => d,
        _ => {
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            return;
        }
    };
    let old_bmp = SelectObject(mem, dib.into());
    let buf = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
    for p in buf.as_chunks_mut::<4>().0 {
        *p = [BG[2], BG[1], BG[0], 255];
    }

    // Status mark.
    let (cx, cy) = (21.0 * scale, h as f32 / 2.0);
    let reach = (9.0 * scale).ceil() as i32;
    for yy in (cy as i32 - reach).max(0)..(cy as i32 + reach).min(h) {
        for xx in (cx as i32 - reach).max(0)..(cx as i32 + reach).min(w) {
            let (dx, dy) = (xx as f32 + 0.5 - cx, yy as f32 + 0.5 - cy);
            if let Some((color, c)) = mark_at(&state, scale, frame, dx, dy) {
                blend(buf, ((yy * w + xx) * 4) as usize, color, c);
            }
        }
    }

    // Text. GDI writes RGB only; alpha is set below.
    let f = font(scale, is_preview(&state));
    let old_font = SelectObject(mem, f.into());
    SetBkMode(mem, TRANSPARENT);
    SetTextColor(
        mem,
        COLORREF(u32::from_le_bytes([TEXT[0], TEXT[1], TEXT[2], 0])),
    );
    let mut text: Vec<u16> = label_of(&state).encode_utf16().collect();
    let px = |v: f32| (v * scale).round() as i32;
    let mut rc = RECT {
        left: px(PAD_LEFT),
        top: 0,
        right: w - px(PAD_RIGHT) + px(4.0),
        bottom: h,
    };
    if !text.is_empty() {
        DrawTextW(
            mem,
            &mut text,
            &mut rc,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
    }
    let _ = GdiFlush();
    SelectObject(mem, old_font);
    let _ = DeleteObject(f.into());

    // Pill shape: anti-aliased edge, a hairline border, premultiplied alpha.
    let (fw, fh) = (w as f32, h as f32);
    let edge = scale.max(1.0);
    for yy in 0..h {
        for xx in 0..w {
            let i = ((yy * w + xx) * 4) as usize;
            let d = pill_distance(xx as f32 + 0.5, yy as f32 + 0.5, fw, fh);
            let cover = clamp01(0.5 - d);
            if cover <= 0.0 {
                buf[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            blend(buf, i, EDGE, clamp01(d + edge + 0.5) * 0.9);
            let a = cover * OPACITY;
            for k in 0..3 {
                buf[i + k] = (buf[i + k] as f32 * a).round() as u8;
            }
            buf[i + 3] = (a * 255.0).round() as u8;
        }
    }

    let blendfn = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    if let Err(e) = UpdateLayeredWindow(
        hwnd,
        Some(screen),
        Some(&POINT {
            x: rect.left,
            y: rect.top,
        }),
        Some(&SIZE { cx: w, cy: h }),
        Some(mem),
        Some(&POINT { x: 0, y: 0 }),
        COLORREF(0),
        Some(&blendfn),
        ULW_ALPHA,
    ) {
        log::warn!("overlay update failed: {e}");
    }
    SelectObject(mem, old_bmp);
    let _ = DeleteObject(dib.into());
    let _ = DeleteDC(mem);
    ReleaseDC(None, screen);
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_REFRESH => {
            refresh(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_SPIN => {
            let working = {
                let mut s = SHARED.lock().unwrap();
                s.frame = s.frame.wrapping_add(1);
                matches!(s.state, Overlay::Working(_))
            };
            if working {
                render(hwnd);
            } else {
                let _ = KillTimer(Some(hwnd), TIMER_SPIN);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_TOAST => {
            let _ = KillTimer(Some(hwnd), TIMER_TOAST);
            let expired = {
                let mut s = SHARED.lock().unwrap();
                let done = s.toast_until.is_some_and(|t| Instant::now() >= t);
                if done {
                    s.state = Overlay::Hidden;
                    s.toast_until = None;
                }
                done
            };
            if expired {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_shows_newest_words() {
        let long = "word ".repeat(40);
        let label = label_of(&Overlay::Listening {
            label: "Listening · Gemini".into(),
            secs: 3,
            preview: long,
        });
        assert!(label.starts_with('…'));
        assert!(label.chars().count() <= PREVIEW_CHARS + 1);
    }

    #[test]
    fn listening_label_has_timer() {
        let label = label_of(&Overlay::Listening {
            label: "Listening · Gemini".into(),
            secs: 65,
            preview: String::new(),
        });
        assert_eq!(label, "Listening · Gemini   1:05");
    }

    #[test]
    fn pill_edges_are_round() {
        // Center is deep inside, a corner pixel is outside, the flat top edge is on the boundary.
        assert!(pill_distance(100.0, 20.0, 200.0, 40.0) < -19.0);
        assert!(pill_distance(0.5, 0.5, 200.0, 40.0) > 5.0);
        assert!(pill_distance(100.0, 0.0, 200.0, 40.0).abs() < 1e-3);
    }

    #[test]
    fn spinner_and_dot_marks() {
        let working = Overlay::Working("Transcribing…".into());
        // On the ring: drawn; at the center: not.
        assert!(mark_at(&working, 1.0, 0, 0.0, 6.0).is_some_and(|(c, a)| c == AMBER && a > 0.9));
        assert!(mark_at(&working, 1.0, 0, 0.0, 0.0).is_none());
        let rec = Overlay::Listening {
            label: String::new(),
            secs: 0,
            preview: String::new(),
        };
        assert_eq!(mark_at(&rec, 1.0, 0, 0.0, 0.0), Some((RED, 1.0)));
        assert_eq!(mark_at(&rec, 1.0, 0, 8.0, 0.0).map(|m| m.1), Some(0.0));
    }
}
