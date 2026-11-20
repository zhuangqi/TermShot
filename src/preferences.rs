use crate::appearance::{
    fill, label, primary_fill, rgb, round, selected_fill, GdiPlus, ACCENT_TEXT, BORDER,
    CONTROL_CORNER, FIELD, FOCUS_BORDER, FOOTER, HOVER, PANEL, PANEL_CORNER, PRESSED, SECONDARY,
    SELECTED_TEXT, TEXT, WINDOW,
};
use crate::storage::{key_label, keys, wide, Hotkey, Settings};
use crate::{alert, control_text, create_control, register_class, startup, with_app, APP_NAME};
use std::io;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Graphics::GdiPlus::PointF;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "TermShot.Preferences";
const WIDTH: i32 = 400;
const HEIGHT: i32 = FOOTER_TOP + 42;
const PAGE_INSET: i32 = 12;
const CONTENT_LEFT: i32 = PAGE_INSET + 12;
const CONTENT_RIGHT: i32 = WIDTH - CONTENT_LEFT;
const ROW_HEIGHT: i32 = 46;
const ROW_COUNT: i32 = 3;
const SHORTCUT_Y: i32 = PAGE_INSET + ROW_HEIGHT / 2;
const RETENTION_Y: i32 = SHORTCUT_Y + ROW_HEIGHT;
const STARTUP_Y: i32 = RETENTION_Y + ROW_HEIGHT;
const FOOTER_TOP: i32 = PAGE_INSET + ROW_COUNT * ROW_HEIGHT + 10;
const FIELD_HEIGHT: i32 = 28;
const CONTROL_DIAMETER: i32 = 14;
const CONTROL_BORDER: COLORREF = rgb(59, 59, 69);
const SELECTED_BORDER: COLORREF = rgb(156, 78, 95);
const ICON_SIZE: i32 = 24;
const LABEL_X: i32 = CONTENT_LEFT + ICON_SIZE + 8;
const LABEL_WIDTH: i32 = 88;
const KEY_WIDTH: i32 = 60;
const CTRL: i32 = 201;
const SHIFT: i32 = 202;
const ALT: i32 = 203;
const KEY: i32 = 204;
const MINUTES: i32 = 205;
const STARTUP: i32 = 206;
const SHORTCUT_LABEL: i32 = 211;
const RETENTION_LABEL: i32 = 212;
const STARTUP_LABEL: i32 = 213;
const MODIFIERS: [(i32, u32); 3] = [(CTRL, MOD_CONTROL), (SHIFT, MOD_SHIFT), (ALT, MOD_ALT)];
const TOGGLE: u32 = BS_AUTOCHECKBOX as u32 | BS_PUSHLIKE as u32 | WS_TABSTOP;
const ACTION: u32 = BS_OWNERDRAW as u32 | WS_TABSTOP;

type Bounds = (i32, i32, i32, i32);
const SHORTCUT_TOP: i32 = SHORTCUT_Y - FIELD_HEIGHT / 2;
const RETENTION_TOP: i32 = RETENTION_Y - FIELD_HEIGHT / 2;
const STARTUP_TOP: i32 = STARTUP_Y - FIELD_HEIGHT / 2;
const SWITCH_WIDTH: i32 = 44;
const KEY_X: i32 = CONTENT_RIGHT - KEY_WIDTH;
const KEY_SEPARATOR: Bounds = (KEY_X - 16, SHORTCUT_TOP, 16, FIELD_HEIGHT);
const NUMBER_FIELD: Bounds = (CONTENT_RIGHT - 118, RETENTION_TOP, 118, FIELD_HEIGHT);
const MINUTES_INPUT: Bounds = (NUMBER_FIELD.0 + 6, RETENTION_TOP, 75, FIELD_HEIGHT);
const MINUTES_UNIT: Bounds = (CONTENT_RIGHT - 33, RETENTION_TOP, 28, FIELD_HEIGHT);
const ACTION_TOP: i32 = FOOTER_TOP + 8;
const ACTION_HEIGHT: i32 = 26;
const SAVE_WIDTH: i32 = 78;
const SAVE_X: i32 = WIDTH - PAGE_INSET - SAVE_WIDTH;
const CANCEL_WIDTH: i32 = 58;
const CONTROLS: [(i32, &str, &str, u32, Bounds); 11] = [
    (
        SHORTCUT_LABEL,
        "STATIC",
        "全局截图",
        0,
        (LABEL_X, SHORTCUT_TOP, LABEL_WIDTH, FIELD_HEIGHT),
    ),
    (
        CTRL,
        "BUTTON",
        "Ctrl",
        TOGGLE,
        (154, SHORTCUT_TOP, 48, FIELD_HEIGHT),
    ),
    (
        SHIFT,
        "BUTTON",
        "Shift",
        TOGGLE,
        (206, SHORTCUT_TOP, 48, FIELD_HEIGHT),
    ),
    (
        ALT,
        "BUTTON",
        "Alt",
        TOGGLE,
        (258, SHORTCUT_TOP, 42, FIELD_HEIGHT),
    ),
    (
        KEY,
        "BUTTON",
        "",
        WS_TABSTOP,
        (KEY_X, SHORTCUT_TOP, KEY_WIDTH, FIELD_HEIGHT),
    ),
    (
        RETENTION_LABEL,
        "STATIC",
        "自动删除截图",
        0,
        (LABEL_X, RETENTION_TOP, LABEL_WIDTH, FIELD_HEIGHT),
    ),
    (
        MINUTES,
        "EDIT",
        "",
        WS_TABSTOP | ES_RIGHT as u32 | ES_NUMBER as u32 | ES_AUTOHSCROLL as u32,
        MINUTES_INPUT,
    ),
    (
        STARTUP_LABEL,
        "STATIC",
        "开机启动",
        0,
        (LABEL_X, STARTUP_TOP, LABEL_WIDTH, FIELD_HEIGHT),
    ),
    (
        STARTUP,
        "BUTTON",
        "开机启动",
        TOGGLE,
        (
            CONTENT_RIGHT - SWITCH_WIDTH,
            STARTUP_TOP,
            SWITCH_WIDTH,
            FIELD_HEIGHT,
        ),
    ),
    (
        IDCANCEL,
        "BUTTON",
        "取消",
        ACTION,
        (
            SAVE_X - CANCEL_WIDTH - 8,
            ACTION_TOP,
            CANCEL_WIDTH,
            ACTION_HEIGHT,
        ),
    ),
    (
        IDOK,
        "BUTTON",
        "保存设置",
        ACTION,
        (SAVE_X, ACTION_TOP, SAVE_WIDTH, ACTION_HEIGHT),
    ),
];

struct Style {
    dpi: u32,
    initial_startup: Option<bool>,
    gdi_plus: Option<GdiPlus>,
    body: HFONT,
    strong: HFONT,
    small: HFONT,
    row: HBRUSH,
    field: HBRUSH,
}

impl Style {
    unsafe fn new(dpi: u32, initial_startup: Option<bool>) -> Self {
        Self {
            dpi,
            initial_startup,
            gdi_plus: GdiPlus::new(),
            body: create_font(12, FW_NORMAL, dpi),
            strong: create_font(13, FW_SEMIBOLD, dpi),
            small: create_font(11, FW_NORMAL, dpi),
            row: CreateSolidBrush(PANEL),
            field: CreateSolidBrush(FIELD),
        }
    }

    unsafe fn round(&self, dc: HDC, bounds: &RECT, diameter: i32, surface: u32, border: u32) {
        if !self
            .gdi_plus
            .as_ref()
            .is_some_and(|renderer| renderer.round(dc, bounds, diameter, surface, border))
        {
            round(dc, bounds, diameter, surface, border);
        }
    }

    unsafe fn stroke(&self, dc: HDC, points: &[(i32, i32)], color: COLORREF) {
        let width = scale(1, self.dpi);
        let points: Vec<_> = points
            .iter()
            .map(|&(x, y)| POINT {
                x: scale(x, self.dpi),
                y: scale(y, self.dpi),
            })
            .collect();
        if self.gdi_plus.as_ref().is_some_and(|renderer| {
            let offset = (width % 2) as f32 / 2.0;
            let points: Vec<_> = points
                .iter()
                .map(|point| PointF {
                    X: point.x as f32 + offset,
                    Y: point.y as f32 + offset,
                })
                .collect();
            renderer.stroke(dc, &points, width as f32, color)
        }) {
            return;
        }
        let pen = CreatePen(PS_SOLID, width, color);
        if !pen.is_null() {
            let previous = SelectObject(dc, pen);
            Polyline(dc, points.as_ptr(), points.len() as i32);
            SelectObject(dc, previous);
            DeleteObject(pen);
        }
    }
}

impl Drop for Style {
    fn drop(&mut self) {
        unsafe {
            for object in [self.body, self.strong, self.small, self.row, self.field] {
                if !object.is_null() {
                    DeleteObject(object);
                }
            }
        }
    }
}

pub fn register(instance: HINSTANCE) -> io::Result<()> {
    register_class(instance, CLASS, Some(window_proc), IDC_ARROW, null_mut())
}

pub fn open(instance: HINSTANCE, initial: Settings) -> io::Result<HWND> {
    let initial_startup = startup::is_enabled().ok();
    unsafe {
        let mut cursor = POINT::default();
        GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return Err(io::Error::last_os_error());
        }
        let work = info.rcWork;
        let style = WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN;
        let ex_style = WS_EX_CONTROLPARENT;
        let hwnd = CreateWindowExW(
            ex_style,
            wide(CLASS).as_ptr(),
            wide(format!("{APP_NAME} 设置")).as_ptr(),
            style,
            work.left,
            work.top,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return Err(io::Error::last_os_error());
        }
        let dpi = GetDpiForWindow(hwnd).max(96);
        let mut frame = rect((0, 0, WIDTH, HEIGHT), dpi);
        AdjustWindowRectExForDpi(&mut frame, style, 0, ex_style, dpi);
        let width = frame.right - frame.left;
        let height = frame.bottom - frame.top;
        SetWindowPos(
            hwnd,
            null_mut(),
            work.left + ((work.right - work.left - width) / 2).max(0),
            work.top + ((work.bottom - work.top - height) / 2).max(0),
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        for (attribute, value) in [
            (DWMWA_USE_IMMERSIVE_DARK_MODE, 1),
            (DWMWA_CAPTION_COLOR, WINDOW),
            (DWMWA_TEXT_COLOR, TEXT),
            (DWMWA_BORDER_COLOR, BORDER),
        ] {
            DwmSetWindowAttribute(hwnd, attribute as u32, (&value as *const u32).cast(), 4);
        }
        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            Box::into_raw(Box::new(Style::new(dpi, initial_startup))) as isize,
        );
        for (id, class, name, style, bounds) in CONTROLS {
            let rect = rect(bounds, dpi);
            let control = create_control(
                hwnd,
                class,
                name,
                style,
                id as usize,
                (
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                ),
            );
            if control.is_null() {
                let error = io::Error::last_os_error();
                DestroyWindow(hwnd);
                return Err(error);
            }
            if matches!(id, CTRL | SHIFT | ALT | KEY | STARTUP | IDOK | IDCANCEL)
                && SetWindowSubclass(control, Some(control_proc), 1, id as usize) == 0
            {
                DestroyWindow(hwnd);
                return Err(io::Error::other("无法初始化设置控件"));
            }
        }
        layout(hwnd);
        for (id, flag) in MODIFIERS {
            let checked = if initial.hotkey.modifiers & flag != 0 {
                BST_CHECKED
            } else {
                BST_UNCHECKED
            };
            SendMessageW(GetDlgItem(hwnd, id), BM_SETCHECK, checked as usize, 0);
        }
        set_primary_key(GetDlgItem(hwnd, KEY), initial.hotkey.key);
        let minutes = GetDlgItem(hwnd, MINUTES);
        SendMessageW(minutes, EM_SETLIMITTEXT, 10, 0);
        SendMessageW(
            minutes,
            EM_SETMARGINS,
            (EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize,
            0,
        );
        SetWindowTextW(
            minutes,
            wide(initial.retention_minutes.to_string()).as_ptr(),
        );
        let startup = GetDlgItem(hwnd, STARTUP);
        match initial_startup {
            Some(true) => {
                SendMessageW(startup, BM_SETCHECK, BST_CHECKED as usize, 0);
            }
            Some(false) => {}
            None => {
                EnableWindow(startup, 0);
            }
        }
        SetWindowPos(
            hwnd,
            null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_SHOWWINDOW,
        );
        SetForegroundWindow(hwnd);
        SetFocus(hwnd);
        Ok(hwnd)
    }
}

unsafe fn set_primary_key(field: HWND, key: u32) {
    SetWindowLongPtrW(field, GWLP_USERDATA, key as isize);
    SetWindowTextW(field, wide(key_label(key)).as_ptr());
    InvalidateRect(field, null(), 0);
}

pub fn handle_key(message: &MSG) -> bool {
    handle_dialog_key(
        with_app(|app| app.preferences).unwrap_or(null_mut()),
        message,
    )
}

fn handle_dialog_key(hwnd: HWND, message: &MSG) -> bool {
    if hwnd.is_null() {
        return false;
    }
    unsafe {
        if message.hwnd != hwnd && IsChild(hwnd, message.hwnd) == 0 {
            return false;
        }
        let key = message.wParam as u16;
        if message.message == WM_KEYDOWN
            && message.hwnd == GetDlgItem(hwnd, KEY)
            && keys().any(|allowed| allowed == u32::from(key))
        {
            set_primary_key(message.hwnd, u32::from(key));
            return true;
        }
        if message.message == WM_KEYDOWN && matches!(key, VK_ESCAPE | VK_RETURN) {
            let command = if key == VK_ESCAPE || message.hwnd == GetDlgItem(hwnd, IDCANCEL) {
                IDCANCEL
            } else {
                IDOK
            };
            PostMessageW(hwnd, WM_COMMAND, command as usize, 0);
            return true;
        }
        IsDialogMessageW(hwnd, message) != 0
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let style = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Style;
    match message {
        WM_PAINT if !style.is_null() => paint_page(hwnd, &*style),
        WM_ERASEBKGND => return 1,
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT if !style.is_null() => {
            let (background, brush) = if message == WM_CTLCOLORSTATIC {
                (PANEL, (*style).row)
            } else {
                (FIELD, (*style).field)
            };
            SetTextColor(wparam as HDC, TEXT);
            SetBkColor(wparam as HDC, background);
            return brush as LRESULT;
        }
        WM_DRAWITEM if lparam != 0 && !style.is_null() => {
            let item = &*(lparam as *const DRAWITEMSTRUCT);
            match item.CtlID as i32 {
                IDOK | IDCANCEL => paint_action(item, &*style),
                _ => return DefWindowProcW(hwnd, message, wparam, lparam),
            }
            return 1;
        }
        WM_COMMAND => match ((wparam & 0xffff) as i32, (wparam >> 16) as u32) {
            (IDOK, BN_CLICKED) if !style.is_null() => save(hwnd, (*style).initial_startup),
            (IDCANCEL, BN_CLICKED) => {
                DestroyWindow(hwnd);
            }
            (id @ (CTRL | SHIFT | ALT | STARTUP), BN_CLICKED) => {
                InvalidateRect(GetDlgItem(hwnd, id), null(), 0);
            }
            (MINUTES, EN_SETFOCUS | EN_KILLFOCUS) if !style.is_null() => {
                InvalidateRect(hwnd, &rect(NUMBER_FIELD, (*style).dpi), 0);
            }
            _ => return DefWindowProcW(hwnd, message, wparam, lparam),
        },
        WM_CLOSE => {
            DestroyWindow(hwnd);
        }
        WM_DPICHANGED if lparam != 0 && !style.is_null() => {
            let dpi = (wparam & 0xffff) as u32;
            let suggested = &*(lparam as *const RECT);
            let old = std::ptr::replace(style, Style::new(dpi, (*style).initial_startup));
            SetWindowPos(
                hwnd,
                null_mut(),
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            layout(hwnd);
            drop(old);
        }
        WM_NCDESTROY => {
            if !style.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(style));
            }
            with_app(|app| {
                if app.preferences == hwnd {
                    app.preferences = null_mut();
                }
            });
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        _ => return DefWindowProcW(hwnd, message, wparam, lparam),
    }
    0
}

unsafe extern "system" fn control_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    id: usize,
) -> LRESULT {
    let id = id as i32;
    match message {
        WM_PAINT if matches!(id, CTRL | SHIFT | ALT | KEY | STARTUP) => {
            let style = GetWindowLongPtrW(GetParent(hwnd), GWLP_USERDATA) as *const Style;
            if !style.is_null() {
                paint_control(hwnd, id, &*style);
                return 0;
            }
        }
        WM_MOUSEMOVE => {
            let mut tracking = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_QUERY,
                ..Default::default()
            };
            TrackMouseEvent(&mut tracking);
            if tracking.hwndTrack != hwnd || tracking.dwFlags & TME_LEAVE == 0 {
                tracking.hwndTrack = hwnd;
                tracking.dwFlags = TME_LEAVE;
                TrackMouseEvent(&mut tracking);
                InvalidateRect(hwnd, null(), 0);
            }
        }
        WM_NCDESTROY => {
            RemoveWindowSubclass(hwnd, Some(control_proc), subclass);
        }
        _ => {}
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if matches!(
        message,
        WM_MOUSELEAVE | WM_SETFOCUS | WM_KILLFOCUS | BM_SETCHECK | BM_SETSTATE
    ) {
        InvalidateRect(hwnd, null(), 0);
    }
    result
}

unsafe fn paint_page(hwnd: HWND, style: &Style) {
    let mut paint = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut paint);
    let dpi = style.dpi;
    let mut client = RECT::default();
    GetClientRect(hwnd, &mut client);
    fill(dc, &client, WINDOW);

    style.round(
        dc,
        &rect(
            (
                PAGE_INSET,
                PAGE_INSET,
                WIDTH - 2 * PAGE_INSET,
                ROW_COUNT * ROW_HEIGHT,
            ),
            dpi,
        ),
        scale(PANEL_CORNER, dpi),
        PANEL,
        BORDER,
    );
    for row in 1..ROW_COUNT {
        let divider = PAGE_INSET + row * ROW_HEIGHT;
        style.stroke(
            dc,
            &[(CONTENT_LEFT, divider), (CONTENT_RIGHT, divider)],
            BORDER,
        );
    }
    paint_row_icons(dc, style);
    text(
        dc,
        style.small,
        "+",
        rect(KEY_SEPARATOR, dpi),
        SECONDARY,
        DT_CENTER,
    );

    let minutes = GetDlgItem(hwnd, MINUTES);
    style.round(
        dc,
        &rect(NUMBER_FIELD, dpi),
        scale(CONTROL_DIAMETER, dpi),
        FIELD,
        if GetFocus() == minutes {
            FOCUS_BORDER
        } else {
            CONTROL_BORDER
        },
    );
    text(
        dc,
        style.body,
        "分钟",
        rect(MINUTES_UNIT, dpi),
        SECONDARY,
        DT_LEFT,
    );

    let footer = scale(FOOTER_TOP, dpi);
    fill(
        dc,
        &RECT {
            top: footer,
            ..client
        },
        FOOTER,
    );
    fill(
        dc,
        &RECT {
            top: footer,
            bottom: footer + 1,
            ..client
        },
        BORDER,
    );
    let hint_top = ACTION_TOP + (ACTION_HEIGHT - 18) / 2;
    for (x, width, key, action) in [(PAGE_INSET, 34, "Enter", "保存"), (84, 28, "Esc", "取消")]
    {
        let keycap = rect((x, hint_top, width, 18), dpi);
        style.round(dc, &keycap, scale(6, dpi), WINDOW, BORDER);
        text(dc, style.small, key, keycap, SECONDARY, DT_CENTER);
        text(
            dc,
            style.small,
            action,
            rect((x + width + 5, hint_top, 26, 18), dpi),
            SECONDARY,
            DT_LEFT,
        );
    }
    EndPaint(hwnd, &paint);
}

unsafe fn paint_row_icons(dc: HDC, style: &Style) {
    let dpi = style.dpi;
    for y in [SHORTCUT_Y, RETENTION_Y, STARTUP_Y] {
        style.round(
            dc,
            &rect((CONTENT_LEFT, y - ICON_SIZE / 2, ICON_SIZE, ICON_SIZE), dpi),
            scale(CONTROL_CORNER, dpi),
            HOVER,
            HOVER,
        );
    }
    let center = CONTENT_LEFT + ICON_SIZE / 2;
    style.round(
        dc,
        &rect((center - 8, SHORTCUT_Y - 5, 16, 10), dpi),
        scale(4, dpi),
        HOVER,
        SECONDARY,
    );
    for x in [center - 5, center - 1, center + 3] {
        style.stroke(
            dc,
            &[(x, SHORTCUT_Y - 2), (x + 2, SHORTCUT_Y - 2)],
            SECONDARY,
        );
    }
    style.stroke(
        dc,
        &[(center - 4, SHORTCUT_Y + 2), (center + 4, SHORTCUT_Y + 2)],
        SECONDARY,
    );
    style.round(
        dc,
        &rect((center - 7, RETENTION_Y - 7, 14, 14), dpi),
        scale(14, dpi),
        HOVER,
        SECONDARY,
    );
    style.stroke(
        dc,
        &[
            (center, RETENTION_Y - 4),
            (center, RETENTION_Y),
            (center + 3, RETENTION_Y + 2),
        ],
        SECONDARY,
    );
    style.round(
        dc,
        &rect((center - 7, STARTUP_Y - 6, 14, 10), dpi),
        scale(3, dpi),
        HOVER,
        SECONDARY,
    );
    style.stroke(
        dc,
        &[(center, STARTUP_Y + 3), (center, STARTUP_Y + 7)],
        SECONDARY,
    );
    style.stroke(
        dc,
        &[(center - 4, STARTUP_Y + 7), (center + 5, STARTUP_Y + 7)],
        SECONDARY,
    );
}

unsafe fn paint_control(hwnd: HWND, id: i32, style: &Style) {
    let mut paint = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut paint);
    let mut client = RECT::default();
    GetClientRect(hwnd, &mut client);
    fill(dc, &client, PANEL);
    if id == STARTUP {
        paint_switch(dc, hwnd, client, style);
    } else {
        let caption = String::from_utf16_lossy(&control_text(hwnd));
        if id == KEY {
            let edge = if GetFocus() == hwnd {
                FOCUS_BORDER
            } else {
                CONTROL_BORDER
            };
            let surface = if is_hovered(hwnd) { HOVER } else { FIELD };
            style.round(
                dc,
                &client,
                scale(CONTROL_DIAMETER, style.dpi),
                surface,
                edge,
            );
            text(dc, style.body, &caption, client, TEXT, DT_CENTER);
        } else {
            paint_modifier(dc, hwnd, client, &caption, style);
        }
    }
    EndPaint(hwnd, &paint);
}

unsafe fn paint_switch(dc: HDC, hwnd: HWND, bounds: RECT, style: &Style) {
    if style.initial_startup.is_none() {
        text(dc, style.small, "不可用", bounds, SECONDARY, DT_CENTER);
        return;
    }
    let dpi = style.dpi;
    let checked = SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
    let pressed = SendMessageW(hwnd, BM_GETSTATE, 0, 0) & BST_PUSHED as isize != 0;
    let hovered = is_hovered(hwnd);
    let (surface, edge, thumb) = if checked {
        let accent = primary_fill(hovered, pressed);
        (accent, accent, ACCENT_TEXT)
    } else {
        let surface = if hovered || pressed { HOVER } else { FIELD };
        (surface, CONTROL_BORDER, SECONDARY)
    };
    let edge = if GetFocus() == hwnd {
        FOCUS_BORDER
    } else {
        edge
    };
    style.round(
        dc,
        &rect((0, 3, SWITCH_WIDTH, 22), dpi),
        scale(22, dpi),
        surface,
        edge,
    );
    let thumb_x = if checked { SWITCH_WIDTH - 18 } else { 4 };
    style.round(
        dc,
        &rect((thumb_x, 7, 14, 14), dpi),
        scale(14, dpi),
        thumb,
        thumb,
    );
}

unsafe fn paint_modifier(dc: HDC, hwnd: HWND, bounds: RECT, caption: &str, style: &Style) {
    let dpi = style.dpi;
    let checked = SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
    let pressed = SendMessageW(hwnd, BM_GETSTATE, 0, 0) & BST_PUSHED as isize != 0;
    let hovered = pressed || is_hovered(hwnd);
    let (surface, edge, ink) = if checked {
        (selected_fill(hovered), SELECTED_BORDER, SELECTED_TEXT)
    } else if hovered {
        (HOVER, CONTROL_BORDER, TEXT)
    } else {
        (FIELD, CONTROL_BORDER, SECONDARY)
    };
    let edge = if GetFocus() == hwnd {
        FOCUS_BORDER
    } else {
        edge
    };
    style.round(dc, &bounds, scale(CONTROL_DIAMETER, dpi), surface, edge);
    let center = FIELD_HEIGHT / 2;
    if checked {
        style.stroke(dc, &[(6, center), (9, center + 3), (14, center - 3)], ink);
    } else {
        style.round(
            dc,
            &rect((7, center - 3, 6, 6), dpi),
            scale(6, dpi),
            surface,
            SECONDARY,
        );
    }
    text(
        dc,
        style.body,
        caption,
        RECT {
            left: scale(16, dpi),
            right: bounds.right - scale(3, dpi),
            ..bounds
        },
        ink,
        DT_CENTER,
    );
}

unsafe fn paint_action(item: &DRAWITEMSTRUCT, style: &Style) {
    let pressed = item.itemState & ODS_SELECTED != 0;
    let focused = item.itemState & ODS_FOCUS != 0;
    let hovered = is_hovered(item.hwndItem);
    let save = item.CtlID as i32 == IDOK;
    let (caption, surface, border, ink) = if save {
        let surface = primary_fill(hovered, pressed);
        ("保存设置", surface, surface, ACCENT_TEXT)
    } else {
        let surface = if pressed {
            PRESSED
        } else if hovered {
            HOVER
        } else {
            PANEL
        };
        ("取消", surface, CONTROL_BORDER, TEXT)
    };
    let border = if focused { FOCUS_BORDER } else { border };
    fill(item.hDC, &item.rcItem, FOOTER);
    style.round(
        item.hDC,
        &item.rcItem,
        scale(CONTROL_DIAMETER, style.dpi),
        surface,
        border,
    );
    text(item.hDC, style.body, caption, item.rcItem, ink, DT_CENTER);
}

unsafe fn is_hovered(hwnd: HWND) -> bool {
    let mut cursor = POINT::default();
    GetCursorPos(&mut cursor) != 0 && WindowFromPoint(cursor) == hwnd
}

unsafe fn text(dc: HDC, font: HFONT, caption: &str, bounds: RECT, color: COLORREF, alignment: u32) {
    let previous = if font.is_null() {
        null_mut()
    } else {
        SelectObject(dc, font)
    };
    label(dc, caption, bounds, color, alignment);
    if !previous.is_null() {
        SelectObject(dc, previous);
    }
}

unsafe fn save(hwnd: HWND, initial_startup: Option<bool>) {
    let mut modifiers = 0;
    for (id, flag) in MODIFIERS {
        if SendMessageW(GetDlgItem(hwnd, id), BM_GETCHECK, 0, 0) == BST_CHECKED as isize {
            modifiers |= flag;
        }
    }
    let hotkey = Hotkey {
        modifiers,
        key: GetWindowLongPtrW(GetDlgItem(hwnd, KEY), GWLP_USERDATA) as u32,
    };
    if !hotkey.valid() {
        alert(hwnd, "请为截图快捷键选择至少一个修饰键和一个主键。");
        SetFocus(GetDlgItem(hwnd, CTRL));
        return;
    }
    let input = GetDlgItem(hwnd, MINUTES);
    let minutes = String::from_utf16_lossy(&control_text(input));
    let Some(retention_minutes) = minutes.parse::<u32>().ok().filter(|&value| value > 0) else {
        alert(hwnd, "请输入大于零的整数分钟数。");
        SetFocus(input);
        return;
    };
    let startup =
        SendMessageW(GetDlgItem(hwnd, STARTUP), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
    let startup_change = initial_startup
        .filter(|&initial| initial != startup)
        .map(|_| startup);
    match with_app(|app| {
        app.apply_settings(
            Settings {
                hotkey,
                retention_minutes,
            },
            startup_change,
        )
    }) {
        Some(Ok(())) => {
            if let Some(owner) = with_app(|app| app.hwnd) {
                crate::update_cleanup(
                    owner,
                    crate::cleanup::Event::RetentionChanged(retention_minutes),
                );
            }
            DestroyWindow(hwnd);
        }
        Some(Err(error)) => alert(hwnd, &error),
        None => alert(hwnd, &format!("{APP_NAME} 暂时无法保存设置。")),
    }
}

unsafe fn layout(hwnd: HWND) {
    let style = &*(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Style);
    let dc = GetDC(hwnd);
    for (id, _, _, _, bounds) in CONTROLS {
        let control = GetDlgItem(hwnd, id);
        let is_label = matches!(id, SHORTCUT_LABEL | RETENTION_LABEL | STARTUP_LABEL);
        let font = if is_label { style.strong } else { style.body };
        SendMessageW(control, WM_SETFONT, font as usize, 0);
        let mut rect = rect(bounds, style.dpi);
        if !dc.is_null() && (is_label || id == MINUTES) {
            let previous = SelectObject(dc, font);
            let mut metrics = TEXTMETRICW::default();
            if GetTextMetricsW(dc, &mut metrics) != 0 {
                let height = metrics.tmHeight.min(rect.bottom - rect.top);
                rect.top += (rect.bottom - rect.top - height) / 2;
                rect.bottom = rect.top + height;
            }
            SelectObject(dc, previous);
        }
        MoveWindow(
            control,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            1,
        );
    }
    if !dc.is_null() {
        ReleaseDC(hwnd, dc);
    }
    RedrawWindow(hwnd, null(), null_mut(), RDW_INVALIDATE | RDW_ALLCHILDREN);
}

unsafe fn create_font(height: i32, weight: u32, dpi: u32) -> HFONT {
    CreateFontW(
        -scale(height, dpi),
        0,
        0,
        0,
        weight as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        DEFAULT_PITCH as u32,
        wide("Microsoft YaHei UI").as_ptr(),
    )
}

fn rect((x, y, width, height): Bounds, dpi: u32) -> RECT {
    RECT {
        left: scale(x, dpi),
        top: scale(y, dpi),
        right: scale(x + width, dpi),
        bottom: scale(y + height, dpi),
    }
}

fn scale(value: i32, dpi: u32) -> i32 {
    (value * dpi as i32 + 48) / 96
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_key_input_and_dialog_commands() {
        unsafe {
            let dialog = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                100,
                50,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            );
            assert!(!dialog.is_null());
            let field = create_control(
                dialog,
                "BUTTON",
                "",
                WS_TABSTOP,
                KEY as usize,
                (0, 0, 60, 28),
            );
            assert!(!field.is_null());
            let press = |vk: u16| {
                handle_dialog_key(
                    dialog,
                    &MSG {
                        hwnd: field,
                        message: WM_KEYDOWN,
                        wParam: usize::from(vk),
                        ..Default::default()
                    },
                )
            };
            for (vk, label) in [(b'A' as u16, "A"), (b'9' as u16, "9"), (VK_F11, "F11")] {
                assert!(press(vk));
                assert_eq!(GetWindowLongPtrW(field, GWLP_USERDATA), vk as isize);
                assert_eq!(String::from_utf16_lossy(&control_text(field)), label);
            }
            for (vk, command) in [(VK_RETURN, IDOK), (VK_ESCAPE, IDCANCEL)] {
                assert!(press(vk));
                let mut message = MSG::default();
                assert_ne!(
                    PeekMessageW(&mut message, dialog, WM_COMMAND, WM_COMMAND, PM_REMOVE),
                    0
                );
                assert_eq!(message.wParam, command as usize);
                assert_eq!(GetWindowLongPtrW(field, GWLP_USERDATA), VK_F11 as isize);
            }
            DestroyWindow(dialog);
        }
    }
}
