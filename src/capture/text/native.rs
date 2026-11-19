use super::{Area, Composition, Font, Point};
use crate::{control_text, create_control};
use std::ops::Range;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::MapWindowPoints;
use windows_sys::Win32::System::Threading::INFINITE;
use windows_sys::Win32::UI::Controls::{EM_GETSEL, EM_SETIMESTATUS, EM_SETLIMITTEXT, EM_SETSEL};
use windows_sys::Win32::UI::Input::Ime::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_RETURN};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub(in crate::capture) const FINISHED: u32 = WM_APP + 10;
pub(in crate::capture) const UPDATED: u32 = WM_APP + 11;
pub(in crate::capture) const IME_POSITION: u32 = WM_APP + 12;
pub(in crate::capture) const CARET_TIMER: usize = 2;

pub(super) struct NativeEdit {
    pub hwnd: HWND,
    pub overlay: HWND,
}

impl NativeEdit {
    pub fn new(overlay: HWND, font: &Font) -> Option<Self> {
        unsafe {
            // Park the input off the canvas so its native background and caret never cover it.
            let hwnd = create_control(
                overlay,
                "EDIT",
                "",
                (ES_MULTILINE | ES_AUTOHSCROLL | ES_AUTOVSCROLL) as u32,
                0,
                (-2, -2, 1, 1),
            );
            if hwnd.is_null() {
                return None;
            }
            if SetWindowSubclass(hwnd, Some(edit_proc), 1, overlay as usize) == 0 {
                DestroyWindow(hwnd);
                return None;
            }
            SendMessageW(hwnd, WM_SETFONT, font.handle as usize, 0);
            SendMessageW(hwnd, EM_SETLIMITTEXT, 160, 0);
            SendMessageW(
                hwnd,
                EM_SETIMESTATUS,
                EMSIS_COMPOSITIONSTRING as WPARAM,
                EIMES_GETCOMPSTRATONCE as LPARAM,
            );
            Some(Self { hwnd, overlay })
        }
    }

    pub fn focus(&self) {
        unsafe { SetFocus(self.hwnd) };
    }

    pub fn close(self) {
        let overlay = self.overlay;
        drop(self);
        unsafe { SetFocus(overlay) };
    }

    pub fn read(&self) -> (Vec<u16>, Range<usize>, Option<Composition>) {
        let units = control_text(self.hwnd);
        let (mut start, mut end) = (0u32, 0u32);
        unsafe {
            SendMessageW(
                self.hwnd,
                EM_GETSEL,
                &mut start as *mut u32 as WPARAM,
                &mut end as *mut u32 as LPARAM,
            );
        }
        let composition = with_ime(self.hwnd, |context| unsafe {
            let length = composition_length(context);
            if length <= 0 {
                return None;
            }
            let mut units = vec![0u16; length as usize / 2];
            let count = ImmGetCompositionStringW(
                context,
                GCS_COMPSTR,
                units.as_mut_ptr().cast(),
                length as u32,
            );
            if count <= 0 {
                return None;
            }
            units.truncate(count as usize / 2);
            let caret = ImmGetCompositionStringW(context, GCS_CURSORPOS, null_mut(), 0);
            Some(Composition {
                units,
                caret: caret.max(0) as usize,
            })
        })
        .flatten();
        (units, start as usize..end as usize, composition)
    }

    pub fn finish_composition(&self) -> Vec<u16> {
        with_ime(self.hwnd, |context| unsafe {
            if composition_length(context) > 0 {
                ImmNotifyIME(context, NI_COMPOSITIONSTR, CPS_COMPLETE, 0);
            }
        });
        control_text(self.hwnd)
    }

    pub fn set_caret(&self, index: usize) {
        unsafe { SendMessageW(self.hwnd, EM_SETSEL, index, index as LPARAM) };
    }

    pub fn place_candidate(&self, point: Point, line_height: i32) {
        with_ime(self.hwnd, |context| unsafe {
            let mut position = POINT {
                x: point.x,
                y: point.y,
            };
            MapWindowPoints(self.overlay, self.hwnd, &mut position, 1);
            let candidate = CANDIDATEFORM {
                dwIndex: 0,
                dwStyle: CFS_EXCLUDE,
                ptCurrentPos: position,
                rcArea: RECT {
                    left: position.x,
                    top: position.y,
                    right: position.x + 1,
                    bottom: position.y + line_height,
                },
            };
            ImmSetCandidateWindow(context, &candidate);
        });
    }

    pub fn restart_blink(&self) {
        unsafe {
            match GetCaretBlinkTime() {
                0 | INFINITE => self.stop_blink(),
                interval => {
                    SetTimer(self.overlay, CARET_TIMER, interval, None);
                }
            }
        }
    }

    pub fn stop_blink(&self) {
        unsafe { KillTimer(self.overlay, CARET_TIMER) };
    }
}

impl Drop for NativeEdit {
    fn drop(&mut self) {
        self.stop_blink();
        unsafe { DestroyWindow(self.hwnd) };
    }
}

fn with_ime<T>(hwnd: HWND, action: impl FnOnce(HIMC) -> T) -> Option<T> {
    unsafe {
        let context = ImmGetContext(hwnd);
        if context.is_null() {
            return None;
        }
        let result = action(context);
        ImmReleaseContext(hwnd, context);
        Some(result)
    }
}

unsafe fn composition_length(context: HIMC) -> i32 {
    ImmGetCompositionStringW(context, GCS_COMPSTR, null_mut(), 0)
}

pub(in crate::capture) fn handle_finish_key(
    message: &MSG,
    overlay_for: impl FnOnce(HWND) -> Option<HWND>,
) -> bool {
    let request = finish_request(message, overlay_for, |source| {
        with_ime(source, |context| unsafe { composition_length(context) > 0 }).unwrap_or(false)
    });
    let Some((overlay, keep)) = request else {
        return false;
    };
    unsafe { SendMessageW(overlay, FINISHED, usize::from(keep), 0) };
    true
}

fn finish_request(
    message: &MSG,
    overlay_for: impl FnOnce(HWND) -> Option<HWND>,
    has_composition: impl FnOnce(HWND) -> bool,
) -> Option<(HWND, bool)> {
    let keep = match (message.message, message.wParam as u16) {
        (WM_KEYDOWN, VK_RETURN) => true,
        (WM_KEYDOWN, VK_ESCAPE) => false,
        _ => return None,
    };
    let overlay = overlay_for(message.hwnd)?;
    // Native EDIT gets the key while there is unfinished IME text.
    (!has_composition(message.hwnd)).then_some((overlay, keep))
}

pub(super) fn report_position(
    point: Point,
    line_height: i32,
    crop: Area,
    desktop: Area,
    info: &mut IMECHARPOSITION,
) {
    let rect = crop.offset(desktop.origin());
    info.pt = POINT {
        x: rect.left + point.x,
        y: rect.top + point.y,
    };
    info.cLineHeight = line_height as u32;
    info.rcDocument = rect.rect();
}

unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    mut lparam: LPARAM,
    _: usize,
    parent: usize,
) -> LRESULT {
    let parent = parent as HWND;
    if message == WM_NCDESTROY {
        RemoveWindowSubclass(hwnd, Some(edit_proc), 1);
    } else if message == WM_IME_SETCONTEXT {
        lparam &= !(ISC_SHOWUICOMPOSITIONWINDOW as isize);
    } else if matches!(
        message,
        WM_IME_STARTCOMPOSITION | WM_IME_ENDCOMPOSITION | WM_IME_COMPOSITIONFULL
    ) || (message == WM_IME_COMPOSITION && lparam as u32 & GCS_RESULTSTR == 0)
    {
        // The canvas draws unfinished text. Native EDIT inserts the finalized IME text.
        PostMessageW(parent, UPDATED, hwnd as WPARAM, 0);
        return 0;
    } else if message == WM_IME_COMPOSITION {
        lparam = GCS_RESULTSTR as LPARAM;
    } else if message == WM_IME_REQUEST
        && wparam == IMR_QUERYCHARPOSITION as usize
        && SendMessageW(parent, IME_POSITION, hwnd as WPARAM, lparam) != 0
    {
        return 1;
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if matches!(
        message,
        WM_KEYDOWN
            | WM_CHAR
            | WM_SETTEXT
            | EM_SETSEL
            | WM_PASTE
            | WM_CUT
            | WM_CLEAR
            | WM_UNDO
            | WM_IME_COMPOSITION
    ) || (message == WM_IME_NOTIFY && wparam as u32 == IMN_OPENCANDIDATE)
    {
        PostMessageW(parent, UPDATED, hwnd as WPARAM, 0);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_and_escape_stay_with_native_edit_during_composition() {
        for (key, keep) in [(VK_RETURN, true), (VK_ESCAPE, false)] {
            let message = MSG {
                message: WM_KEYDOWN,
                wParam: usize::from(key),
                ..MSG::default()
            };
            let overlay_for = |_| Some(null_mut());
            assert_eq!(finish_request(&message, overlay_for, |_| true), None);
            assert_eq!(
                finish_request(&message, overlay_for, |_| false),
                Some((null_mut(), keep))
            );
            assert_eq!(
                finish_request(&message, |_| None, |_| panic!("unrelated input")),
                None
            );
        }
    }
}
