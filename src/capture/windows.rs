use super::{Area, Point};
use std::ptr::null_mut;
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub(super) fn visible_bounds(desktop: Area) -> Vec<Area> {
    let mut bounds = Vec::<Area>::new();
    unsafe {
        EnumWindows(
            Some(collect_window),
            &mut bounds as *mut Vec<Area> as LPARAM,
        )
    };
    bounds
        .into_iter()
        .filter_map(|area| {
            let clipped = area.intersection(desktop)?;
            Some(clipped.offset(Point {
                x: -desktop.left,
                y: -desktop.top,
            }))
        })
        .collect()
}

unsafe extern "system" fn collect_window(hwnd: HWND, data: LPARAM) -> BOOL {
    if IsWindowVisible(hwnd) == 0
        || IsIconic(hwnd) != 0
        || GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT != 0
    {
        return 1;
    }
    let mut name = [0; 128];
    let length = GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32) as usize;
    if matches!(
        String::from_utf16_lossy(&name[..length]).as_str(),
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    ) {
        return 1;
    }
    let mut cloaked = 0u32;
    DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED as u32,
        (&mut cloaked as *mut u32).cast(),
        size_of::<u32>() as u32,
    );
    if cloaked != 0 {
        return 1;
    }
    let mut rect = RECT::default();
    if DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS as u32,
        (&mut rect as *mut RECT).cast(),
        size_of::<RECT>() as u32,
    ) < 0
        && GetWindowRect(hwnd, &mut rect) == 0
    {
        return 1;
    }
    let windows = &mut *(data as *mut Vec<Area>);
    windows.push(Area {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    });
    1
}

pub(super) fn pointer(hwnd: HWND) -> Option<Point> {
    let mut point = POINT::default();
    (unsafe { GetCursorPos(&mut point) } != 0).then(|| {
        unsafe {
            windows_sys::Win32::Graphics::Gdi::MapWindowPoints(null_mut(), hwnd, &mut point, 1)
        };
        Point {
            x: point.x,
            y: point.y,
        }
    })
}
