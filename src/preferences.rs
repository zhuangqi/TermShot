use crate::appearance::{self, ACCENT, ACCENT_HOVER, BORDER, CANVAS, MUTED, NAVY, TINT, WHITE};
use crate::storage::{key_label, keys, wide, Hotkey, Settings};
use crate::{alert, control_text, create_control, register_class, with_app, APP_NAME};
use std::io;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "CliScreenshot.Preferences";
const FONT_PROPERTY: &str = "CliScreenshot.Preferences.Font";
const CTRL: usize = 201;
const SHIFT: usize = 202;
const ALT: usize = 203;
const KEY: usize = 204;
const MINUTES: usize = 205;
const SHORTCUT_HINT: usize = 212;
const RETENTION_HINT: usize = 215;
const CONTROL_LAYOUTS: [(usize, (i32, i32, i32, i32)); 12] = [
    (211, (42, 117, 196, 22)),
    (CTRL, (258, 132, 47, 28)),
    (SHIFT, (311, 132, 48, 28)),
    (ALT, (365, 132, 43, 28)),
    (KEY, (424, 132, 78, 220)),
    (SHORTCUT_HINT, (42, 149, 210, 20)),
    (213, (42, 226, 220, 22)),
    (MINUTES, (380, 239, 74, 28)),
    (214, (468, 242, 35, 23)),
    (RETENTION_HINT, (42, 257, 230, 20)),
    (IDOK as usize, (414, 318, 112, 36)),
    (IDCANCEL as usize, (312, 318, 88, 36)),
];

pub fn register(instance: HINSTANCE) -> io::Result<()> {
    register_class(instance, CLASS, Some(window_proc), IDC_ARROW, null_mut())
}

pub fn open(instance: HINSTANCE, initial: Settings) -> io::Result<HWND> {
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
        let ex_style = WS_EX_CONTROLPARENT | WS_EX_DLGMODALFRAME;
        let title = wide("CliScreenshot 设置");
        let hwnd = CreateWindowExW(
            ex_style,
            wide(CLASS).as_ptr(),
            title.as_ptr(),
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
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: scale(548, dpi),
            bottom: scale(374, dpi),
        };
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
        let definitions = [
            ("STATIC", "截图快捷键", 0),
            (
                "BUTTON",
                "Ctrl",
                WS_TABSTOP | BS_AUTOCHECKBOX as u32 | BS_PUSHLIKE as u32,
            ),
            (
                "BUTTON",
                "Shift",
                WS_TABSTOP | BS_AUTOCHECKBOX as u32 | BS_PUSHLIKE as u32,
            ),
            (
                "BUTTON",
                "Alt",
                WS_TABSTOP | BS_AUTOCHECKBOX as u32 | BS_PUSHLIKE as u32,
            ),
            (
                "COMBOBOX",
                "",
                WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
            ),
            ("STATIC", "快捷键在工具运行时全局生效。", 0),
            ("STATIC", "截图自动删除时间", 0),
            (
                "EDIT",
                "",
                WS_TABSTOP | WS_BORDER | ES_NUMBER as u32 | ES_AUTOHSCROLL as u32,
            ),
            ("STATIC", "分钟", 0),
            ("STATIC", "从截图保存时开始计时。", 0),
            ("BUTTON", "保存设置", WS_TABSTOP | BS_OWNERDRAW as u32),
            ("BUTTON", "取消", WS_TABSTOP | BS_OWNERDRAW as u32),
        ];
        for ((id, bounds), (class, label, style)) in CONTROL_LAYOUTS.into_iter().zip(definitions) {
            if create_control(hwnd, class, label, style, id, scaled_bounds(bounds, dpi)).is_null() {
                let error = io::Error::last_os_error();
                DestroyWindow(hwnd);
                return Err(error);
            }
        }
        layout(hwnd, dpi);
        for (id, flag) in [(CTRL, MOD_CONTROL), (SHIFT, MOD_SHIFT), (ALT, MOD_ALT)] {
            let checked = if initial.hotkey.modifiers & flag != 0 {
                BST_CHECKED
            } else {
                BST_UNCHECKED
            };
            SendMessageW(
                GetDlgItem(hwnd, id as i32),
                BM_SETCHECK,
                checked as usize,
                0,
            );
        }
        let key_list = GetDlgItem(hwnd, KEY as i32);
        for (index, key) in keys().enumerate() {
            let label = wide(key_label(key));
            SendMessageW(key_list, CB_ADDSTRING, 0, label.as_ptr() as isize);
            if key == initial.hotkey.key {
                SendMessageW(key_list, CB_SETCURSEL, index, 0);
            }
        }
        let minutes = GetDlgItem(hwnd, MINUTES as i32);
        SendMessageW(minutes, EM_SETLIMITTEXT, 10, 0);
        SetWindowTextW(
            minutes,
            wide(initial.retention_minutes.to_string()).as_ptr(),
        );
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
        SetFocus(GetDlgItem(hwnd, CTRL as i32));
        Ok(hwnd)
    }
}

pub fn handle_key(message: &MSG) -> bool {
    let hwnd = with_app(|app| app.preferences).unwrap_or(null_mut());
    if hwnd.is_null() {
        return false;
    }
    unsafe {
        if message.hwnd != hwnd && IsChild(hwnd, message.hwnd) == 0 {
            return false;
        }
        if message.message == WM_KEYDOWN && message.wParam as u16 == VK_ESCAPE {
            let list = GetDlgItem(hwnd, KEY as i32);
            if SendMessageW(list, CB_GETDROPPEDSTATE, 0, 0) == 0 {
                DestroyWindow(hwnd);
                return true;
            }
        }
        if message.message == WM_KEYDOWN && message.wParam as u16 == VK_RETURN {
            let list = GetDlgItem(hwnd, KEY as i32);
            if SendMessageW(list, CB_GETDROPPEDSTATE, 0, 0) == 0 {
                let command = if message.hwnd == GetDlgItem(hwnd, IDCANCEL) {
                    IDCANCEL
                } else {
                    IDOK
                };
                PostMessageW(hwnd, WM_COMMAND, command as usize, 0);
                return true;
            }
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
    match message {
        WM_PAINT => paint_background(hwnd),
        WM_ERASEBKGND => return 1,
        WM_CTLCOLORSTATIC => {
            let dc = wparam as HDC;
            let control = lparam as HWND;
            let id = GetDlgCtrlID(control) as usize;
            let color = if matches!(id, SHORTCUT_HINT | RETENTION_HINT) {
                MUTED
            } else {
                NAVY
            };
            SetTextColor(dc, color);
            SetBkColor(dc, WHITE);
            return GetStockObject(WHITE_BRUSH) as LRESULT;
        }
        WM_DRAWITEM if lparam != 0 => {
            let item = &*(lparam as *const DRAWITEMSTRUCT);
            if matches!(item.CtlID as i32, IDOK | IDCANCEL) {
                paint_button(hwnd, item);
                return 1;
            }
        }
        WM_COMMAND if wparam == IDOK as usize => save(hwnd),
        WM_COMMAND if wparam == IDCANCEL as usize => {
            DestroyWindow(hwnd);
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
        }
        WM_DPICHANGED if lparam != 0 => {
            let rect = &*(lparam as *const RECT);
            SetWindowPos(
                hwnd,
                null_mut(),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            layout(hwnd, (wparam & 0xffff) as u32);
        }
        WM_NCDESTROY => {
            let font = RemovePropW(hwnd, wide(FONT_PROPERTY).as_ptr());
            if !font.is_null() {
                DeleteObject(font);
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

unsafe fn paint_background(hwnd: HWND) {
    let mut paint = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut paint);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let mut client = RECT::default();
    GetClientRect(hwnd, &mut client);
    appearance::fill(dc, &client, CANVAS);
    appearance::fill(dc, &bounds(0, 0, 548, 82, dpi), NAVY);
    let size = scale(39, dpi);
    let icon = crate::app_icon(GetModuleHandleW(null()), size);
    if !icon.is_null() {
        DrawIconEx(
            dc,
            scale(24, dpi),
            scale(21, dpi),
            icon,
            size,
            size,
            0,
            null_mut(),
            DI_NORMAL,
        );
        DestroyIcon(icon);
    }
    let title_font = create_font(26, FW_BOLD, dpi);
    let previous = if title_font.is_null() {
        null_mut()
    } else {
        SelectObject(dc, title_font)
    };
    appearance::label(dc, "截图设置", bounds(79, 18, 300, 46, dpi), WHITE, DT_LEFT);
    if !title_font.is_null() {
        SelectObject(dc, previous);
        DeleteObject(title_font);
    }
    for top in [101, 210] {
        let card = bounds(22, top, 504, 89, dpi);
        appearance::round(dc, &card, scale(16, dpi), WHITE, BORDER);
    }
    EndPaint(hwnd, &paint);
}

unsafe fn paint_button(hwnd: HWND, item: &DRAWITEMSTRUCT) {
    let is_save = item.CtlID as i32 == IDOK;
    let pressed = item.itemState & ODS_SELECTED != 0;
    let (text, fill, edge, color) = if is_save {
        let fill = if pressed { ACCENT_HOVER } else { ACCENT };
        ("保存设置", fill, fill, WHITE)
    } else {
        ("取消", if pressed { TINT } else { WHITE }, BORDER, NAVY)
    };
    let dpi = GetDpiForWindow(hwnd).max(96);
    appearance::fill(item.hDC, &item.rcItem, CANVAS);
    appearance::round(item.hDC, &item.rcItem, scale(10, dpi), fill, edge);
    let font = GetPropW(hwnd, wide(FONT_PROPERTY).as_ptr());
    let previous = if font.is_null() {
        null_mut()
    } else {
        SelectObject(item.hDC, font)
    };
    appearance::label(item.hDC, text, item.rcItem, color, DT_CENTER);
    if !font.is_null() {
        SelectObject(item.hDC, previous);
    }
    if item.itemState & ODS_FOCUS != 0 {
        let mut focus = item.rcItem;
        InflateRect(&mut focus, -4, -4);
        DrawFocusRect(item.hDC, &focus);
    }
}

unsafe fn save(hwnd: HWND) {
    let mut modifiers = 0;
    for (id, flag) in [(CTRL, MOD_CONTROL), (SHIFT, MOD_SHIFT), (ALT, MOD_ALT)] {
        if SendMessageW(GetDlgItem(hwnd, id as i32), BM_GETCHECK, 0, 0) == BST_CHECKED as isize {
            modifiers |= flag;
        }
    }
    let index = SendMessageW(GetDlgItem(hwnd, KEY as i32), CB_GETCURSEL, 0, 0);
    let hotkey = Hotkey {
        modifiers,
        key: keys().nth(index as usize).unwrap_or_default(),
    };
    if !hotkey.valid() {
        alert(hwnd, "请为截图快捷键选择至少一个修饰键和一个主键。");
        return;
    }
    let input = GetDlgItem(hwnd, MINUTES as i32);
    let minutes = String::from_utf16_lossy(&control_text(input));
    let Some(retention_minutes) = minutes.parse::<u32>().ok().filter(|&value| value > 0) else {
        alert(hwnd, "请输入大于零的整数分钟数。");
        SetFocus(input);
        return;
    };
    match with_app(|app| {
        app.apply_settings(Settings {
            hotkey,
            retention_minutes,
        })
    }) {
        Some(Ok(())) => {
            DestroyWindow(hwnd);
        }
        Some(Err(error)) => alert(hwnd, &error),
        None => alert(hwnd, &format!("{APP_NAME} 暂时无法保存设置。")),
    }
}

unsafe fn layout(hwnd: HWND, dpi: u32) {
    let font = create_font(15, FW_NORMAL, dpi);
    let property = wide(FONT_PROPERTY);
    let previous = GetPropW(hwnd, property.as_ptr());
    let use_font = !font.is_null() && SetPropW(hwnd, property.as_ptr(), font) != 0;
    for (id, bounds) in CONTROL_LAYOUTS {
        let control = GetDlgItem(hwnd, id as i32);
        if control.is_null() {
            continue;
        }
        let (x, y, width, height) = scaled_bounds(bounds, dpi);
        MoveWindow(control, x, y, width, height, 1);
        if use_font {
            SendMessageW(control, WM_SETFONT, font as usize, 1);
        }
    }
    if use_font && !previous.is_null() {
        DeleteObject(previous);
    }
    if !use_font && !font.is_null() {
        DeleteObject(font);
    }
    InvalidateRect(hwnd, null(), 0);
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

fn bounds(x: i32, y: i32, width: i32, height: i32, dpi: u32) -> RECT {
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

fn scaled_bounds((x, y, width, height): (i32, i32, i32, i32), dpi: u32) -> (i32, i32, i32, i32) {
    (
        scale(x, dpi),
        scale(y, dpi),
        scale(width, dpi),
        scale(height, dpi),
    )
}
