#![windows_subsystem = "windows"]

mod appearance;
mod background;
mod capture;
mod preferences;
mod storage;

use background::{Background, Outcome};
use std::cell::RefCell;
use std::io;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::time::Instant;
use storage::{wide, ClipboardError, Hotkey, Settings, Storage};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const APP_NAME: &str = "CliScreenshot";
const ICON_RESOURCE: *const u16 = 101usize as *const u16;
const CONTROLLER_CLASS: &str = "CliScreenshot.Controller";
const WM_TRAY: u32 = WM_APP + 1;
const WM_CAPTURE_FINISHED: u32 = WM_APP + 2;
const WM_OPEN_SETTINGS: u32 = WM_APP + 3;
const CLEANUP_TIMER: usize = 1;
const CLIPBOARD_TIMER: usize = 2;
const CLIPBOARD_RETRIES: u8 = 5;
const CLIPBOARD_RETRY_MS: u32 = 50;
const TRAY_SETTINGS: usize = 101;
const TRAY_FOLDER: usize = 102;
const TRAY_EXIT: usize = 103;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

struct App {
    hwnd: HWND,
    instance: HINSTANCE,
    tray_icon: HICON,
    storage: Storage,
    settings: Settings,
    preferences: HWND,
    hotkey_id: Option<i32>,
    cleanup_enabled: bool,
    cleanup_pending: bool,
    cleanup_again: bool,
    cleanup_deadline: Option<Instant>,
    background: Background,
    clipboard: Option<PendingClipboard>,
    exiting: bool,
    taskbar_created: u32,
}

struct PendingClipboard {
    path: PathBuf,
    retries: u8,
}

fn main() {
    if let Err(error) = run() {
        alert(null_mut(), &format!("无法启动截图工具。\n{error}"));
    }
}

fn run() -> io::Result<()> {
    unsafe {
        let mutex = CreateMutexW(null(), 0, wide("Local\\CliScreenshot.Instance").as_ptr());
        if mutex.is_null() {
            return Err(io::Error::last_os_error());
        }
        let already_running = GetLastError() == ERROR_ALREADY_EXISTS;
        let _instance_lock = InstanceLock(mutex);
        if already_running {
            let running = FindWindowW(wide(CONTROLLER_CLASS).as_ptr(), null());
            if !running.is_null() {
                PostMessageW(running, WM_OPEN_SETTINGS, 0, 0);
            }
            return Ok(());
        }
        let storage = Storage::open()?;
        let settings = storage.load_settings();
        let instance = GetModuleHandleW(null());
        register_class(
            instance,
            CONTROLLER_CLASS,
            Some(controller_proc),
            IDC_ARROW,
            null_mut(),
        )?;
        capture::register(instance)?;
        preferences::register(instance)?;
        let hwnd = CreateWindowExW(
            0,
            wide(CONTROLLER_CLASS).as_ptr(),
            wide(APP_NAME).as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
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
        let background = Background::start(
            hwnd,
            storage.clone(),
            settings
                .as_ref()
                .ok()
                .map(|settings| settings.retention_minutes),
        )?;
        let mut app = App {
            hwnd,
            instance,
            tray_icon: app_icon(instance, GetSystemMetrics(SM_CXSMICON)),
            storage,
            settings: settings.as_ref().copied().unwrap_or_default(),
            preferences: null_mut(),
            hotkey_id: None,
            cleanup_enabled: settings.is_ok(),
            cleanup_pending: false,
            cleanup_again: false,
            cleanup_deadline: None,
            background,
            clipboard: None,
            exiting: false,
            taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
        };
        if !app.show_tray() {
            return Err(io::Error::other("无法创建系统托盘图标"));
        }
        let hotkey_ready = app.register_hotkey(app.settings.hotkey, 1);
        if hotkey_ready {
            app.hotkey_id = Some(1);
        }
        let hotkey = app.settings.hotkey.label();
        APP.with(|slot| slot.replace(Some(app)));
        request_cleanup(hwnd);
        if let Err(error) = settings {
            alert(
                hwnd,
                &format!("无法读取设置。请在托盘的「设置」中保存以恢复自动清理。\n{error}"),
            );
        }
        if !hotkey_ready {
            alert(
                hwnd,
                &format!("无法注册 {hotkey}。请在托盘的「设置」中更换快捷键。"),
            );
        }
        let mut message = MSG::default();
        loop {
            match GetMessageW(&mut message, null_mut(), 0, 0) {
                -1 => return Err(io::Error::last_os_error()),
                0 => return Ok(()),
                _ => (),
            }
            if capture::handle_edit_key(&message) || preferences::handle_key(&message) {
                continue;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

struct InstanceLock(HANDLE);

impl Drop for InstanceLock {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn with_app<R>(action: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|slot| {
        slot.try_borrow_mut()
            .ok()
            .and_then(|mut slot| slot.as_mut().map(action))
    })
}

unsafe extern "system" fn controller_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_HOTKEY => {
            let target = with_app(|app| {
                app.hotkey_id
                    .filter(|&id| !app.exiting && id as usize == wparam)
                    .map(|_| app.instance)
            })
            .flatten();
            if let Some(instance) = target {
                if let Err(error) = capture::start(instance, hwnd) {
                    alert(hwnd, &format!("无法截图。\n{error}"));
                }
            }
        }
        WM_CAPTURE_FINISHED => {
            match capture::complete(with_app(|app| app.background.save_budget()).unwrap_or(0)) {
                Ok(Some(image)) => {
                    if let Some(Err(error)) = with_app(|app| app.background.save(image)) {
                        alert(hwnd, &format!("无法保存截图。\n{error}"));
                    }
                }
                Ok(None) => (),
                Err(error) => alert(hwnd, &format!("无法生成截图。\n{error}")),
            }
        }
        background::READY => receive_background(hwnd),
        WM_TIMER if wparam == CLEANUP_TIMER => {
            request_cleanup(hwnd);
        }
        WM_TIMER if wparam == CLIPBOARD_TIMER => {
            copy_saved_path(hwnd);
            exit_when_ready(hwnd);
        }
        WM_POWERBROADCAST if wparam == 7 || wparam == 18 => {
            request_cleanup(hwnd);
        }
        WM_TIMECHANGE => {
            request_cleanup(hwnd);
        }
        WM_TRAY if lparam as u32 == WM_RBUTTONUP => show_menu(hwnd),
        WM_TRAY if lparam as u32 == WM_LBUTTONDBLCLK => open_preferences(),
        WM_OPEN_SETTINGS => open_preferences(),
        WM_COMMAND => match wparam & 0xffff {
            TRAY_SETTINGS => open_preferences(),
            TRAY_FOLDER => open_folder(hwnd),
            TRAY_EXIT => {
                with_app(|app| app.exiting = true);
                exit_when_ready(hwnd);
            }
            _ => (),
        },
        WM_DESTROY => {
            with_app(|app| {
                Shell_NotifyIconW(NIM_DELETE, &app.tray_data());
                if !app.tray_icon.is_null() {
                    DestroyIcon(app.tray_icon);
                    app.tray_icon = null_mut();
                }
                if let Some(id) = app.hotkey_id {
                    UnregisterHotKey(hwnd, id);
                }
                KillTimer(hwnd, CLEANUP_TIMER);
                KillTimer(hwnd, CLIPBOARD_TIMER);
            });
            APP.with(|slot| {
                if let Ok(mut app) = slot.try_borrow_mut() {
                    app.take();
                }
            });
            PostQuitMessage(0);
        }
        _ => {
            with_app(|app| {
                if message == app.taskbar_created && app.taskbar_created != 0 {
                    app.show_tray();
                }
            });
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
    }
    0
}

impl App {
    fn tray_data(&self) -> NOTIFYICONDATAW {
        let mut icon = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: if self.tray_icon.is_null() {
                unsafe { LoadIconW(null_mut(), IDI_APPLICATION) }
            } else {
                self.tray_icon
            },
            ..Default::default()
        };
        let title = wide(APP_NAME);
        icon.szTip[..title.len()].copy_from_slice(&title);
        icon
    }

    fn show_tray(&self) -> bool {
        unsafe { Shell_NotifyIconW(NIM_ADD, &self.tray_data()) != 0 }
    }

    fn register_hotkey(&self, hotkey: Hotkey, id: i32) -> bool {
        unsafe { RegisterHotKey(self.hwnd, id, hotkey.modifiers | MOD_NOREPEAT, hotkey.key) != 0 }
    }

    fn request_cleanup(&mut self) -> io::Result<()> {
        unsafe {
            KillTimer(self.hwnd, CLEANUP_TIMER);
        }
        self.cleanup_deadline = None;
        if !self.cleanup_enabled {
            return Ok(());
        }
        if self.cleanup_pending {
            self.cleanup_again = true;
            return Ok(());
        }
        self.background.clean()?;
        self.cleanup_pending = true;
        Ok(())
    }

    fn cleanup_finished(&mut self, deadline: Option<Instant>) -> io::Result<()> {
        self.cleanup_pending = false;
        if std::mem::take(&mut self.cleanup_again) {
            return self.request_cleanup();
        }
        if let Some(deadline) = deadline {
            return self.schedule_cleanup(deadline);
        }
        Ok(())
    }

    fn schedule_cleanup(&mut self, deadline: Instant) -> io::Result<()> {
        if self
            .cleanup_deadline
            .is_some_and(|current| current <= deadline)
        {
            return Ok(());
        }
        let delay = deadline.saturating_duration_since(Instant::now());
        let wait = delay.as_millis().clamp(10, 0x7fff_ffff) as u32;
        if unsafe { SetTimer(self.hwnd, CLEANUP_TIMER, wait, None) } == 0 {
            return Err(io::Error::last_os_error());
        }
        self.cleanup_deadline = Some(deadline);
        Ok(())
    }

    fn capture_saved(&mut self, saved: storage::SavedCapture) -> io::Result<()> {
        let deadline = self
            .cleanup_enabled
            .then(|| Instant::now().checked_add(saved.expires_in(self.settings.retention_minutes)))
            .flatten();
        self.clipboard = Some(PendingClipboard {
            path: saved.path,
            retries: CLIPBOARD_RETRIES,
        });
        if let Some(deadline) = deadline.filter(|_| !self.cleanup_pending) {
            self.schedule_cleanup(deadline)?;
        }
        Ok(())
    }

    fn copy_saved_path(&mut self) -> Result<(), (PathBuf, io::Error)> {
        unsafe { KillTimer(self.hwnd, CLIPBOARD_TIMER) };
        let Some(mut pending) = self.clipboard.take() else {
            return Ok(());
        };
        match storage::copy_path(self.hwnd, &pending.path) {
            Ok(()) => Ok(()),
            Err(ClipboardError::Busy(error)) if pending.retries > 0 => {
                pending.retries -= 1;
                if unsafe { SetTimer(self.hwnd, CLIPBOARD_TIMER, CLIPBOARD_RETRY_MS, None) } == 0 {
                    return Err((pending.path, error));
                }
                self.clipboard = Some(pending);
                Ok(())
            }
            Err(ClipboardError::Busy(error) | ClipboardError::Failed(error)) => {
                Err((pending.path, error))
            }
        }
    }

    fn apply_settings(&mut self, settings: Settings) -> Result<(), String> {
        let new_id = if self.hotkey_id.is_none() || settings.hotkey != self.settings.hotkey {
            let id = if self.hotkey_id == Some(1) { 2 } else { 1 };
            if !self.register_hotkey(settings.hotkey, id) {
                return Err(format!(
                    "无法注册 {}，该快捷键可能正被占用。",
                    settings.hotkey.label()
                ));
            }
            Some(id)
        } else {
            None
        };
        if let Err(error) = self.storage.save_settings(settings) {
            if let Some(id) = new_id {
                unsafe {
                    UnregisterHotKey(self.hwnd, id);
                }
            }
            return Err(format!("无法保存设置。\n{error}"));
        }
        if let Some(id) = new_id {
            if let Some(old) = self.hotkey_id.replace(id) {
                unsafe {
                    UnregisterHotKey(self.hwnd, old);
                }
            }
        }
        self.settings = settings;
        self.cleanup_enabled = true;
        self.background.set_retention(settings.retention_minutes);
        Ok(())
    }
}

fn request_cleanup(hwnd: HWND) {
    if let Some(Err(error)) = with_app(App::request_cleanup) {
        alert(hwnd, &format!("无法安排截图清理。\n{error}"));
    }
}

fn receive_background(hwnd: HWND) {
    while let Some(outcome) = with_app(|app| app.background.receive()).flatten() {
        match outcome {
            Outcome::Saved(result) => match result {
                Ok(saved) => {
                    let cleanup = with_app(|app| app.capture_saved(saved));
                    copy_saved_path(hwnd);
                    if let Some(Err(error)) = cleanup {
                        alert(hwnd, &format!("无法安排截图清理。\n{error}"));
                    }
                }
                Err(error) => alert(hwnd, &format!("无法保存截图。\n{error}")),
            },
            Outcome::Cleaned(deadline) => {
                if let Some(Err(error)) = with_app(|app| app.cleanup_finished(deadline)) {
                    alert(hwnd, &format!("无法安排截图清理。\n{error}"));
                }
            }
        }
    }
    exit_when_ready(hwnd);
}

fn copy_saved_path(hwnd: HWND) {
    if let Some(Err((path, error))) = with_app(App::copy_saved_path) {
        alert(
            hwnd,
            &format!("截图已保存，复制路径失败。\n{}\n{error}", path.display()),
        );
    }
}

fn exit_when_ready(hwnd: HWND) {
    if with_app(|app| app.exiting && !app.background.is_saving() && app.clipboard.is_none())
        == Some(true)
    {
        unsafe { DestroyWindow(hwnd) };
    }
}

fn open_preferences() {
    let Some((owner, instance, settings, existing)) =
        with_app(|app| (app.hwnd, app.instance, app.settings, app.preferences))
    else {
        return;
    };
    if !existing.is_null() && unsafe { IsWindow(existing) != 0 } {
        unsafe {
            ShowWindow(existing, SW_RESTORE);
            SetForegroundWindow(existing);
        }
        return;
    }
    match preferences::open(instance, settings) {
        Ok(hwnd) => {
            with_app(|app| app.preferences = hwnd);
        }
        Err(error) => alert(owner, &format!("无法打开设置。\n{error}")),
    }
}

fn open_folder(owner: HWND) {
    let Some(folder) = with_app(|app| app.storage.captures.clone()) else {
        return;
    };
    let result = std::fs::create_dir_all(&folder).and_then(|()| {
        let status = unsafe {
            ShellExecuteW(
                owner,
                wide("open").as_ptr(),
                wide(&folder).as_ptr(),
                null(),
                null(),
                SW_SHOWNORMAL,
            )
        };
        if status as usize <= 32 {
            return Err(io::Error::other(format!(
                "ShellExecuteW {}",
                status as usize
            )));
        }
        Ok(())
    });
    if let Err(error) = result {
        alert(
            owner,
            &format!("无法打开截图目录。\n{}\n{error}", folder.display()),
        );
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    AppendMenuW(menu, MF_STRING, TRAY_SETTINGS, wide("设置").as_ptr());
    AppendMenuW(menu, MF_STRING, TRAY_FOLDER, wide("打开截图目录").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, TRAY_EXIT, wide("退出").as_ptr());
    let mut cursor = POINT::default();
    if GetCursorPos(&mut cursor) != 0 {
        SetForegroundWindow(hwnd);
        let selected = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            cursor.x,
            cursor.y,
            0,
            hwnd,
            null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        if selected != 0 {
            PostMessageW(hwnd, WM_COMMAND, selected as usize, 0);
        }
    }
    DestroyMenu(menu);
}

fn alert(owner: HWND, message: &str) {
    unsafe {
        MessageBoxW(
            owner,
            wide(message).as_ptr(),
            wide(APP_NAME).as_ptr(),
            MB_OK | MB_ICONWARNING | MB_SETFOREGROUND,
        );
    }
}

fn register_class(
    instance: HINSTANCE,
    name: &str,
    handler: WNDPROC,
    cursor: *const u16,
    background: HBRUSH,
) -> io::Result<()> {
    let name = wide(name);
    let class = WNDCLASSW {
        style: CS_DBLCLKS,
        hIcon: unsafe { LoadIconW(instance, ICON_RESOURCE) },
        lpfnWndProc: handler,
        hInstance: instance,
        lpszClassName: name.as_ptr(),
        hCursor: unsafe { LoadCursorW(null_mut(), cursor) },
        hbrBackground: background,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn app_icon(instance: HINSTANCE, size: i32) -> HICON {
    unsafe {
        LoadImageW(
            instance,
            ICON_RESOURCE,
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
    }
}

unsafe fn create_control(
    parent: HWND,
    class: &str,
    label: &str,
    style: u32,
    id: usize,
    bounds: (i32, i32, i32, i32),
) -> HWND {
    let (x, y, width, height) = bounds;
    CreateWindowExW(
        0,
        wide(class).as_ptr(),
        wide(label).as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        x,
        y,
        width,
        height,
        parent,
        id as HMENU,
        GetModuleHandleW(null()),
        null(),
    )
}

fn control_text(control: HWND) -> Vec<u16> {
    unsafe {
        let mut buffer = vec![0; GetWindowTextLengthW(control) as usize + 1];
        let length = GetWindowTextW(control, buffer.as_mut_ptr(), buffer.len() as i32);
        buffer.truncate(length as usize);
        buffer
    }
}
