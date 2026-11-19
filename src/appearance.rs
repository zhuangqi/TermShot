use crate::storage::wide;
use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::*;

pub const NAVY: u32 = rgb(24, 38, 64);
pub const ACCENT: u32 = rgb(35, 87, 211);
pub const ACCENT_HOVER: u32 = rgb(24, 68, 179);
pub const CANVAS: u32 = rgb(245, 247, 252);
pub const BORDER: u32 = rgb(222, 228, 239);
pub const MUTED: u32 = rgb(83, 99, 123);
pub const TINT: u32 = rgb(232, 239, 255);
pub const WHITE: u32 = rgb(255, 255, 255);

pub const fn rgb(red: u32, green: u32, blue: u32) -> u32 {
    red | (green << 8) | (blue << 16)
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
