use super::{Area, Font, Point};
use crate::{appearance::ACCENT, control_text, create_control};
use std::ops::Range;
use std::ptr::null_mut;
use unicode_segmentation::UnicodeSegmentation;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Controls::{EM_GETSEL, EM_SETIMESTATUS, EM_SETLIMITTEXT, EM_SETSEL};
use windows_sys::Win32::UI::Input::Ime::*;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub(super) const UPDATED: u32 = WM_APP + 11;
pub(super) const IME_POSITION: u32 = WM_APP + 12;

pub(super) struct TextMark {
    pub origin: Point,
    layout: TextLayout,
}

struct TextLayout {
    units: Vec<u16>,
    lines: Vec<TextLine>,
    width: i32,
    line_height: i32,
}

struct TextLine {
    stops: Vec<(usize, i32)>,
}

impl TextLine {
    fn start(&self) -> usize {
        self.stops[0].0
    }

    fn end(&self) -> (usize, i32) {
        *self.stops.last().unwrap()
    }

    fn x_at(&self, index: usize) -> i32 {
        let stop = self.stops.partition_point(|&(offset, _)| offset <= index);
        self.stops[stop.saturating_sub(1)].1
    }
}

impl TextLayout {
    fn new(dc: HDC, font: &Font, mut units: Vec<u16>, limit: i32) -> Self {
        // Keep UTF-16 offsets aligned with the native edit's selection positions.
        for unit in &mut units {
            if *unit == b'\t' as u16 {
                *unit = b' ' as u16;
            }
        }
        let text = String::from_utf16_lossy(&units);
        let mut lines = Vec::new();
        let mut line = TextLine {
            stops: vec![(0, 0)],
        };
        let mut offset = 0;
        let previous = unsafe { SelectObject(dc, font.handle) };
        for grapheme in text.graphemes(true) {
            let end = offset + grapheme.encode_utf16().count();
            if matches!(grapheme, "\n" | "\r" | "\r\n") {
                lines.push(line);
                line = TextLine {
                    stops: vec![(end, 0)],
                };
            } else {
                let mut width = measure(dc, &units[line.start()..end]);
                if width > limit && offset > line.start() {
                    lines.push(line);
                    line = TextLine {
                        stops: vec![(offset, 0)],
                    };
                    width = measure(dc, &units[offset..end]);
                }
                line.stops.push((end, width));
            }
            offset = end;
        }
        unsafe { SelectObject(dc, previous) };
        lines.push(line);
        let width = lines
            .iter()
            .map(|line| line.end().1)
            .max()
            .unwrap_or(0)
            .clamp(font.chinese_width.min(limit), limit);
        Self {
            units,
            lines,
            width,
            line_height: font.line_height,
        }
    }

    fn caret(&self, index: usize) -> Point {
        let row = self
            .lines
            .partition_point(|line| line.start() <= index)
            .saturating_sub(1);
        Point {
            x: self.lines[row].x_at(index).min(self.width - 1),
            y: row as i32 * self.line_height,
        }
    }

    fn index_at(&self, point: Point) -> usize {
        let row = (point.y.max(0) / self.line_height) as usize;
        let line = &self.lines[row.min(self.lines.len() - 1)];
        // Choose the nearest character boundary on the clicked line.
        line.stops
            .iter()
            .min_by_key(|&&(_, x)| (x - point.x).abs())
            .map_or(line.start(), |&(index, _)| index)
    }

    fn height(&self) -> i32 {
        self.lines.len() as i32 * self.line_height
    }
}

pub(super) fn measure(dc: HDC, text: &[u16]) -> i32 {
    let mut size = SIZE::default();
    if !text.is_empty() {
        unsafe { GetTextExtentPoint32W(dc, text.as_ptr(), text.len() as i32, &mut size) };
    }
    size.cx
}

impl TextMark {
    pub fn new(dc: HDC, font: &Font, crop: Area, anchor: Point, text: Vec<u16>) -> Self {
        let x = anchor.x.min((crop.width() - font.chinese_width).max(0));
        let layout = TextLayout::new(dc, font, text, crop.width() - x);
        let mut mark = Self {
            origin: Point { x, y: anchor.y },
            layout,
        };
        mark.origin = mark.clamp_origin(mark.origin, crop);
        mark
    }

    pub fn clamp_origin(&self, point: Point, crop: Area) -> Point {
        Point {
            x: point.x.clamp(0, (crop.width() - self.layout.width).max(0)),
            y: point
                .y
                .clamp(0, (crop.height() - self.layout.height()).max(0)),
        }
    }

    pub fn bounds(&self) -> Area {
        Area {
            left: self.origin.x,
            top: self.origin.y,
            right: self.origin.x + self.layout.width,
            bottom: self.origin.y + self.layout.height(),
        }
    }

    pub unsafe fn paint(&self, dc: HDC, font: &Font, origin: Point, color: COLORREF) {
        let old_font = SelectObject(dc, font.handle);
        let old_mode = SetBkMode(dc, TRANSPARENT as i32);
        let old_color = SetTextColor(dc, color);
        for (row, line) in self.layout.lines.iter().enumerate() {
            let units = &self.layout.units[line.start()..line.end().0];
            if !units.is_empty() {
                TextOutW(
                    dc,
                    origin.x,
                    origin.y + row as i32 * self.layout.line_height,
                    units.as_ptr(),
                    units.len() as i32,
                );
            }
        }
        SetTextColor(dc, old_color);
        SetBkMode(dc, old_mode);
        SelectObject(dc, old_font);
    }
}

pub(super) struct TextInput {
    pub hwnd: HWND,
    anchor: Point,
    pub draft: TextMark,
    selection: Range<usize>,
    composition: Option<Range<usize>>,
    caret: usize,
}

impl TextInput {
    pub fn new(overlay: HWND, dc: HDC, font: &Font, crop: Area, anchor: Point) -> Option<Self> {
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
            Some(Self {
                hwnd,
                anchor,
                draft: TextMark::new(dc, font, crop, anchor, Vec::new()),
                selection: 0..0,
                composition: None,
                caret: 0,
            })
        }
    }

    pub fn refresh(&mut self, overlay: HWND, dc: HDC, font: &Font, crop: Area) {
        let mut units = control_text(self.hwnd);
        let (mut start, mut end) = (0u32, 0u32);
        unsafe {
            SendMessageW(
                self.hwnd,
                EM_GETSEL,
                &mut start as *mut u32 as WPARAM,
                &mut end as *mut u32 as LPARAM,
            );
        }
        self.selection = (start as usize).min(units.len())..(end as usize).min(units.len());
        self.caret = self.selection.end;
        self.composition = None;
        unsafe {
            let context = ImmGetContext(self.hwnd);
            if !context.is_null() {
                let length = ImmGetCompositionStringW(context, GCS_COMPSTR, null_mut(), 0);
                if length > 0 {
                    let mut composing = vec![0u16; length as usize / 2];
                    let read = ImmGetCompositionStringW(
                        context,
                        GCS_COMPSTR,
                        composing.as_mut_ptr().cast(),
                        length as u32,
                    );
                    if read > 0 {
                        composing.truncate(read as usize / 2);
                        let start = self.selection.start;
                        let cursor =
                            ImmGetCompositionStringW(context, GCS_CURSORPOS, null_mut(), 0);
                        self.caret = start + (cursor.max(0) as usize).min(composing.len());
                        self.composition = Some(start..start + composing.len());
                        units.splice(self.selection.clone(), composing);
                    }
                }
                ImmReleaseContext(self.hwnd, context);
            }
        }
        self.draft = TextMark::new(dc, font, crop, self.anchor, units);
        self.place_candidate(overlay, crop);
    }

    pub fn set_caret(&self, point: Point) {
        if self.composition.is_none() {
            let local = Point {
                x: point.x - self.draft.origin.x,
                y: point.y - self.draft.origin.y,
            };
            let index = self.draft.layout.index_at(local);
            unsafe { SendMessageW(self.hwnd, EM_SETSEL, index, index as LPARAM) };
        }
    }

    pub fn finish(&self, dc: HDC, font: &Font, crop: Area) -> Option<TextMark> {
        unsafe {
            let context = ImmGetContext(self.hwnd);
            if !context.is_null() {
                if ImmGetCompositionStringW(context, GCS_COMPSTR, null_mut(), 0) > 0 {
                    ImmNotifyIME(context, NI_COMPOSITIONSTR, CPS_COMPLETE, 0);
                }
                ImmReleaseContext(self.hwnd, context);
            }
        }
        let units = control_text(self.hwnd);
        (!String::from_utf16_lossy(&units).trim().is_empty())
            .then(|| TextMark::new(dc, font, crop, self.anchor, units))
    }

    fn caret_position(&self, index: usize, crop: Area) -> Point {
        let offset = self.draft.layout.caret(index);
        Point {
            x: self.draft.origin.x + offset.x,
            y: (self.draft.origin.y + offset.y)
                .min((crop.height() - self.draft.layout.line_height).max(0)),
        }
    }

    fn place_candidate(&self, overlay: HWND, crop: Area) {
        let point = self.caret_position(self.caret, crop);
        unsafe {
            let context = ImmGetContext(self.hwnd);
            if context.is_null() {
                return;
            }
            let mut position = POINT {
                x: crop.left + point.x,
                y: crop.top + point.y,
            };
            MapWindowPoints(overlay, self.hwnd, &mut position, 1);
            let candidate = CANDIDATEFORM {
                dwIndex: 0,
                dwStyle: CFS_EXCLUDE,
                ptCurrentPos: position,
                rcArea: RECT {
                    left: position.x,
                    top: position.y,
                    right: position.x + 1,
                    bottom: position.y + self.draft.layout.line_height,
                },
            };
            ImmSetCandidateWindow(context, &candidate);
            ImmReleaseContext(self.hwnd, context);
        }
    }

    pub fn ime_position(&self, crop: Area, desktop: Area, info: &mut IMECHARPOSITION) {
        let index = self.composition.as_ref().map_or(self.caret, |range| {
            range.start + (info.dwCharPos as usize).min(range.len())
        });
        let position = self.caret_position(index, crop);
        let rect = crop.offset(Point {
            x: desktop.left,
            y: desktop.top,
        });
        info.pt = POINT {
            x: rect.left + position.x,
            y: rect.top + position.y,
        };
        info.cLineHeight = self.draft.layout.line_height as u32;
        info.rcDocument = rect.rect();
    }

    pub unsafe fn paint(&self, dc: HDC, font: &Font, crop: Area, color: COLORREF) {
        let layout = &self.draft.layout;
        let origin = self.draft.origin;
        let area = self.draft.bounds();
        let brush = CreateSolidBrush(ACCENT);
        if !brush.is_null() {
            FrameRect(dc, &area.rect(), brush);
            DeleteObject(brush);
        }
        self.draft.paint(dc, font, origin, color);
        let underlined = self.composition.as_ref().unwrap_or(&self.selection);
        if !underlined.is_empty() {
            for (row, line) in layout.lines.iter().enumerate() {
                let start = underlined.start.max(line.start());
                let end = underlined.end.min(line.end().0);
                if start < end {
                    let y = origin.y + (row as i32 + 1) * layout.line_height - 2;
                    for x in line.x_at(start)..line.x_at(end).min(layout.width) {
                        SetPixelV(dc, origin.x + x, y, ACCENT);
                    }
                }
            }
        }
        let caret = self.caret_position(self.caret, crop);
        for y in caret.y + 2..caret.y + layout.line_height - 2 {
            SetPixelV(dc, caret.x, y, color);
        }
    }
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
        // The canvas draws the unfinished text. The native edit only inserts IME results.
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
    ) || (message == WM_IME_NOTIFY && wparam == IMN_OPENCANDIDATE as usize)
    {
        PostMessageW(parent, UPDATED, hwnd as WPARAM, 0);
    }
    result
}
