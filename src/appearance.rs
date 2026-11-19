use crate::storage::wide;
use windows_sys::Win32::Foundation::{COLORREF, RECT};
use windows_sys::Win32::Graphics::Gdi::*;

mod smooth;
pub use smooth::{argb, GdiPlus};

pub const WHITE: u32 = rgb(255, 255, 255);
pub const WINDOW: COLORREF = rgb(24, 24, 28);
pub const PANEL: COLORREF = rgb(32, 32, 37);
pub const FOOTER: COLORREF = rgb(21, 21, 25);
pub const FIELD: COLORREF = rgb(26, 26, 31);
pub const BORDER: COLORREF = rgb(52, 52, 61);
pub const CONTROL_BORDER: COLORREF = rgb(65, 65, 76);
pub const HOVER: COLORREF = rgb(43, 43, 51);
pub const PRESSED: COLORREF = rgb(58, 58, 69);
pub const SELECTED_BORDER: COLORREF = rgb(204, 98, 117);
pub const SELECTED_TEXT: COLORREF = rgb(255, 219, 225);
pub const FOCUS_BORDER: COLORREF = rgb(242, 192, 205);
pub const TEXT: COLORREF = rgb(243, 242, 246);
pub const SECONDARY: COLORREF = rgb(174, 173, 186);
pub const MUTED: COLORREF = rgb(118, 116, 132);
pub const ACCENT: COLORREF = rgb(255, 103, 116);
pub const ACCENT_HOVER: COLORREF = rgb(255, 131, 142);
pub const ACCENT_TEXT: COLORREF = rgb(38, 13, 21);
pub const PANEL_CORNER: i32 = 12;
pub const CONTROL_CORNER: i32 = 7;

pub const fn rgb(red: u32, green: u32, blue: u32) -> u32 {
    red | (green << 8) | (blue << 16)
}

pub fn primary_fill(hovered: bool, pressed: bool) -> COLORREF {
    if pressed {
        rgb(227, 84, 98)
    } else if hovered {
        ACCENT_HOVER
    } else {
        ACCENT
    }
}

pub fn selected_fill(hovered: bool) -> COLORREF {
    if hovered {
        rgb(119, 54, 69)
    } else {
        rgb(102, 47, 60)
    }
}

pub unsafe fn fill(dc: HDC, rect: &RECT, color: u32) {
    let brush = CreateSolidBrush(color);
    if !brush.is_null() {
        FillRect(dc, rect, brush);
        DeleteObject(brush);
    }
}

pub unsafe fn round(dc: HDC, rect: &RECT, radius: i32, fill: u32, border: u32) {
    let brush = CreateSolidBrush(fill);
    let pen = CreatePen(PS_SOLID, 1, border);
    if !brush.is_null() && !pen.is_null() {
        let old_brush = SelectObject(dc, brush);
        let old_pen = SelectObject(dc, pen);
        RoundRect(
            dc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            radius,
            radius,
        );
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
    }
    if !pen.is_null() {
        DeleteObject(pen);
    }
    if !brush.is_null() {
        DeleteObject(brush);
    }
}

pub unsafe fn label(dc: HDC, text: &str, mut bounds: RECT, color: u32, flags: u32) {
    let text = wide(text);
    let mode = SetBkMode(dc, TRANSPARENT as i32);
    let previous = SetTextColor(dc, color);
    DrawTextW(
        dc,
        text.as_ptr(),
        text.len() as i32 - 1,
        &mut bounds,
        flags | DT_NOPREFIX | DT_SINGLELINE | DT_VCENTER,
    );
    SetTextColor(dc, previous);
    SetBkMode(dc, mode);
}
