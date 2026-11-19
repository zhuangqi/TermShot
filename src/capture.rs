use crate::appearance::rgb;
use crate::storage::wide;
use crate::{register_class, APP_NAME, WM_CAPTURE_FINISHED};
use std::cell::{OnceCell, RefCell};
use std::io;
use std::ops::{Add, Sub};
use std::ptr::{null, null_mut};
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Controls::{HOVER_DEFAULT, WM_MOUSEHOVER, WM_MOUSELEAVE};
use windows_sys::Win32::UI::Input::Ime::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

mod annotations;
mod render;
mod strokes;
mod text;
mod toolbar;
mod windows;
use annotations::{Annotations, Mark, MarkDrag, Tool};
use render::{update_rectangles, Bitmap, Font};
use text::TextEditor;
use toolbar::Button;

const CLASS: &str = "TermShot.Overlay";
const CLICK_TIMER: usize = 1;
const INK: u32 = rgb(231, 43, 52);
const SELECTION_BORDER: COLORREF = rgb(72, 220, 134);

thread_local! {
    static EDITOR: RefCell<Option<Editor>> = const { RefCell::new(None) };
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Point {
    x: i32,
    y: i32,
}

impl Add for Point {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            x: self.x + other.x,
            y: self.y + other.y,
        }
    }
}

impl Sub for Point {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self {
            x: self.x - other.x,
            y: self.y - other.y,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Area {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Area {
    fn from_drag(start: Point, end: Point) -> Option<Self> {
        if start.x == end.x || start.y == end.y {
            return None;
        }
        Some(Self {
            left: start.x.min(end.x),
            top: start.y.min(end.y),
            right: start.x.max(end.x) + 1,
            bottom: start.y.max(end.y) + 1,
        })
    }

    fn width(self) -> i32 {
        self.right - self.left
    }
    fn height(self) -> i32 {
        self.bottom - self.top
    }

    fn contains(self, point: Point) -> bool {
        point.x >= self.left && point.x < self.right && point.y >= self.top && point.y < self.bottom
    }

    fn intersection(self, other: Self) -> Option<Self> {
        let area = Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        (area.width() > 0 && area.height() > 0).then_some(area)
    }

    fn outside(self, other: Option<Self>) -> impl Iterator<Item = Self> {
        let pieces = if let Some(overlap) = other.and_then(|other| self.intersection(other)) {
            [
                Some(Self {
                    bottom: overlap.top,
                    ..self
                }),
                Some(Self {
                    top: overlap.bottom,
                    ..self
                }),
                Some(Self {
                    top: overlap.top,
                    bottom: overlap.bottom,
                    right: overlap.left,
                    ..self
                }),
                Some(Self {
                    top: overlap.top,
                    bottom: overlap.bottom,
                    left: overlap.right,
                    ..self
                }),
            ]
        } else {
            [Some(self), None, None, None]
        };
        pieces
            .into_iter()
            .flatten()
            .filter(|piece| piece.width() > 0 && piece.height() > 0)
    }

    fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    fn pixel(point: Point) -> Self {
        Self {
            left: point.x,
            top: point.y,
            right: point.x + 1,
            bottom: point.y + 1,
        }
    }

    fn padded(self, margin: i32) -> Self {
        Self {
            left: self.left - margin,
            top: self.top - margin,
            right: self.right + margin,
            bottom: self.bottom + margin,
        }
    }

    fn origin(self) -> Point {
        Point {
            x: self.left,
            y: self.top,
        }
    }

    fn from_rect(rect: RECT) -> Self {
        Self {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }

    fn offset(self, point: Point) -> Self {
        Self {
            left: self.left + point.x,
            top: self.top + point.y,
            right: self.right + point.x,
            bottom: self.bottom + point.y,
        }
    }

    fn local(self, point: Point) -> Point {
        Point {
            x: (point.x - self.left).clamp(0, self.width() - 1),
            y: (point.y - self.top).clamp(0, self.height() - 1),
        }
    }

    fn local_bounds(self) -> Self {
        Self {
            left: 0,
            top: 0,
            right: self.width(),
            bottom: self.height(),
        }
    }

    fn rect(self) -> RECT {
        RECT {
            left: self.left,
            top: self.top,
            right: self.right,
            bottom: self.bottom,
        }
    }
}

enum DragKind {
    Region { box_started: bool },
    Draw(Tool),
    Pen(strokes::LivePen),
    Mark(MarkDrag),
    Toolbar { origin: Point },
}

struct Drag {
    start: Point,
    end: Point,
    kind: DragKind,
}

impl Drag {
    fn update(&mut self, end: Point, crop: Option<Area>) {
        self.end = end;
        match &mut self.kind {
            DragKind::Region { box_started } => {
                *box_started |= unsafe {
                    (end.x - self.start.x).abs() >= GetSystemMetrics(SM_CXDRAG).max(1)
                        || (end.y - self.start.y).abs() >= GetSystemMetrics(SM_CYDRAG).max(1)
                };
            }
            DragKind::Pen(stroke) => {
                if let Some(crop) = crop {
                    stroke.push(crop.local(end));
                }
            }
            _ => {}
        }
    }

    fn delta(&self) -> Point {
        self.end - self.start
    }

    fn drawn_mark(&self, crop: Area, color: COLORREF) -> Option<Mark> {
        let DragKind::Draw(tool) = self.kind else {
            return None;
        };
        let start = crop.local(self.start);
        let end = crop.local(self.end);
        match tool {
            Tool::Rectangle => Area::from_drag(start, end).map(|area| Mark::rectangle(area, color)),
            Tool::Arrow if start != end => Some(Mark::arrow(start, end, color)),
            _ => None,
        }
    }
}

struct Editor {
    hwnd: HWND,
    owner: HWND,
    previous: HWND,
    desktop: Area,
    monitors: Vec<Area>,
    windows: Vec<Area>,
    original: Bitmap,
    frame: Option<Bitmap>,
    antialias: OnceCell<Option<strokes::Antialias>>,
    selection: Option<Area>,
    toolbar: Option<Point>,
    palette_open: bool,
    hovered: Option<Button>,
    tooltip: Option<toolbar::Tooltip>,
    pointer: Option<Point>,
    pending_click: Option<Button>,
    drag: Option<Drag>,
    tool: Tool,
    color: COLORREF,
    annotations: Annotations,
    text: TextEditor,
    finished: Option<bool>,
    save_busy: bool,
}

pub use render::Image;

pub fn register(instance: HINSTANCE) -> io::Result<()> {
    register_class(instance, CLASS, Some(window_proc), IDC_CROSS, null_mut())
}

pub fn start(instance: HINSTANCE, owner: HWND) -> io::Result<()> {
    if EDITOR.with(|slot| slot.borrow().is_some()) {
        return Ok(());
    }
    unsafe {
        let previous = GetForegroundWindow();
        let mut monitors: Vec<Area> = Vec::new();
        if EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(collect_monitor),
            &mut monitors as *mut Vec<Area> as LPARAM,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let desktop = monitors
            .iter()
            .copied()
            .reduce(|a, b| Area {
                left: a.left.min(b.left),
                top: a.top.min(b.top),
                right: a.right.max(b.right),
                bottom: a.bottom.max(b.bottom),
            })
            .ok_or_else(|| io::Error::other("没有可用的显示器"))?;
        let windows = windows::visible_bounds(desktop);
        let original = Bitmap::capture(desktop, &monitors)?;
        let frame = Bitmap::preview(desktop)?;
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            wide(CLASS).as_ptr(),
            wide(format!("{APP_NAME} 截图")).as_ptr(),
            WS_POPUP | WS_CLIPCHILDREN,
            desktop.left,
            desktop.top,
            desktop.width(),
            desktop.height(),
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return Err(io::Error::last_os_error());
        }
        EDITOR.with(|slot| {
            slot.replace(Some(Editor {
                hwnd,
                owner,
                previous,
                desktop,
                monitors,
                windows,
                original,
                frame: Some(frame),
                antialias: OnceCell::new(),
                selection: None,
                toolbar: None,
                palette_open: false,
                hovered: None,
                tooltip: toolbar::Tooltip::new(hwnd, instance),
                pointer: None,
                pending_click: None,
                drag: None,
                tool: Tool::Rectangle,
                color: INK,
                annotations: Annotations::default(),
                text: TextEditor::default(),
                finished: None,
                save_busy: false,
            }))
        });
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
        if let Some(point) = windows::pointer(hwnd) {
            edit(|editor| editor.mouse_move(point));
        }
        UpdateWindow(hwnd);
        Ok(())
    }
}

unsafe extern "system" fn collect_monitor(
    _: HMONITOR,
    _: HDC,
    bounds: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let rect = &*bounds;
    if rect.left < rect.right && rect.top < rect.bottom {
        let monitors = &mut *(data as *mut Vec<Area>);
        monitors.push(Area {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        });
    }
    1
}

pub fn complete(save_budget: usize) -> io::Result<Option<Image>> {
    let Some(mut editor) = EDITOR.with(|slot| slot.take()) else {
        return Ok(None);
    };
    let save = editor.finished == Some(true);
    if save && editor.exceeds_save_budget(save_budget) {
        editor.finished = None;
        editor.save_busy = true;
        unsafe { InvalidateRect(editor.hwnd, null(), 0) };
        EDITOR.with(|slot| slot.replace(Some(editor)));
        return Ok(None);
    }
    unsafe {
        let was_foreground = GetForegroundWindow() == editor.hwnd;
        editor.text.shutdown();
        DestroyWindow(editor.hwnd);
        if was_foreground && !editor.previous.is_null() && IsWindow(editor.previous) != 0 {
            SetForegroundWindow(editor.previous);
        }
    }
    if save {
        editor.export(save_budget).map(Some)
    } else {
        Ok(None)
    }
}

pub fn handle_edit_key(message: &MSG) -> bool {
    text::handle_finish_key(message, |source| {
        EDITOR.with(|slot| slot.borrow().as_ref()?.text.overlay_for(source))
    })
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_PAINT {
        let regions = update_rectangles(hwnd);
        let mut paint = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut paint);
        let regions = regions
            .as_deref()
            .unwrap_or(std::slice::from_ref(&paint.rcPaint));
        with_editor(|editor| editor.paint(dc, regions));
        EndPaint(hwnd, &paint);
        return 0;
    }
    match message {
        WM_ERASEBKGND => return 1,
        WM_LBUTTONDOWN => edit(|editor| editor.mouse_down(editor.pointer(lparam))),
        WM_MOUSEMOVE => edit(|editor| editor.mouse_move(editor.pointer(lparam))),
        WM_LBUTTONUP => edit(|editor| editor.mouse_up(editor.pointer(lparam))),
        WM_LBUTTONDBLCLK => edit(|editor| editor.finish(true)),
        WM_TIMER if wparam == CLICK_TIMER => edit(Editor::apply_pending_click),
        WM_TIMER if wparam == text::CARET_TIMER => edit(|editor| editor.text.blink()),
        WM_MOUSEHOVER => edit(|editor| editor.show_tooltip(editor.pointer(lparam))),
        WM_MOUSELEAVE => edit(|editor| {
            editor.hide_tooltip();
            editor.pointer = None;
            editor.hovered = None;
        }),
        WM_SETCURSOR if wparam as HWND == hwnd => {
            with_editor(|editor| editor.set_cursor(windows::pointer(hwnd)));
            return 1;
        }
        WM_CAPTURECHANGED => edit(|editor| {
            editor.drag = None;
        }),
        WM_KEYDOWN => edit(|editor| editor.key_down(wparam as u16)),
        WM_IME_SETCONTEXT => {
            return DefWindowProcW(
                hwnd,
                message,
                wparam,
                lparam & !(ISC_SHOWUICOMPOSITIONWINDOW as isize),
            );
        }
        text::FINISHED => edit(|editor| {
            editor.close_text(wparam != 0);
        }),
        text::UPDATED => edit(|editor| editor.text.refresh(editor.original.dc, wparam as HWND)),
        text::IME_POSITION if lparam != 0 => {
            let mut handled = false;
            with_editor(|editor| {
                handled = editor.text.ime_position(
                    wparam as HWND,
                    editor.desktop,
                    &mut *(lparam as *mut IMECHARPOSITION),
                );
            });
            return LRESULT::from(handled);
        }
        WM_DISPLAYCHANGE | WM_CLOSE => edit(|editor| editor.finish(false)),
        WM_ACTIVATE if wparam as u16 == WA_INACTIVE as u16 => edit(|editor| editor.finish(false)),
        _ => return DefWindowProcW(hwnd, message, wparam, lparam),
    }
    0
}

fn with_editor(action: impl FnOnce(&mut Editor)) {
    EDITOR.with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut() {
            if let Some(editor) = slot.as_mut().filter(|editor| editor.finished.is_none()) {
                action(editor);
            }
        }
    });
}

fn edit(action: impl FnOnce(&mut Editor)) {
    with_editor(|editor| {
        let before = editor.preview_snapshot();
        action(editor);
        if editor.finished.is_none() {
            let hwnd = editor.hwnd;
            editor.repaint_changes(before, |area| unsafe {
                InvalidateRect(hwnd, &area.rect(), 0);
            });
        }
    });
}

impl Editor {
    fn pointer(&self, position: LPARAM) -> Point {
        Point {
            x: (position as i16 as i32).clamp(0, self.desktop.width() - 1),
            y: ((position >> 16) as i16 as i32).clamp(0, self.desktop.height() - 1),
        }
    }

    fn mouse_down(&mut self, point: Point) {
        self.hide_tooltip();
        self.apply_pending_click();
        if self.finished.is_some() || self.drag.is_some() {
            return;
        }
        let Some(crop) = self.selection else {
            self.begin_drag(point, DragKind::Region { box_started: false });
            return;
        };
        if let Some(layout) = self.toolbar_layout() {
            if let Some(button) = layout.hit(point) {
                self.toolbar_click(button, point);
                return;
            }
            if layout.contains(point) {
                return;
            }
        }
        self.palette_open = false;
        if !crop.contains(point) {
            self.close_text(true);
            return;
        }
        let local = crop.local(point);
        if self.text.click(local) || !self.close_text(true) {
            return;
        }

        match self.tool {
            Tool::Select => {
                if let Some(drag) = self.annotations.begin_transform(local, crop.local_bounds()) {
                    self.begin_drag(point, DragKind::Mark(drag));
                }
            }
            Tool::Text => self.text.open(self.hwnd, self.original.dc, crop, local),
            Tool::Pen => self.begin_drag(point, DragKind::Pen(strokes::LivePen::new(local))),
            tool => self.begin_drag(point, DragKind::Draw(tool)),
        }
    }

    fn begin_drag(&mut self, point: Point, kind: DragKind) {
        self.hovered = None;
        self.drag = Some(Drag {
            start: point,
            end: point,
            kind,
        });
        unsafe { SetCapture(self.hwnd) };
        self.set_cursor(Some(point));
    }

    fn mouse_move(&mut self, point: Point) {
        if self.pointer.replace(point).is_none() {
            self.track_mouse(TME_LEAVE);
        }
        if let Some(drag) = self.drag.as_mut() {
            drag.update(point, self.selection);
            if let DragKind::Toolbar { origin } = drag.kind {
                let position = origin + drag.delta();
                self.toolbar = Some(self.clamp_toolbar(position, point));
            }
        } else {
            let hovered = self.toolbar_layout().and_then(|layout| layout.hit(point));
            if hovered != self.hovered {
                self.hide_tooltip();
                self.hovered = hovered;
                if hovered.and_then(Button::tooltip_text).is_some() {
                    self.track_mouse(TME_HOVER);
                }
            }
        }
        self.set_cursor(Some(point));
    }

    fn track_mouse(&self, flags: TRACKMOUSEEVENT_FLAGS) {
        let mut tracking = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: flags,
            hwndTrack: self.hwnd,
            dwHoverTime: HOVER_DEFAULT,
        };
        unsafe { TrackMouseEvent(&mut tracking) };
    }

    fn show_tooltip(&mut self, point: Point) {
        let Some(layout) = self.toolbar_layout().filter(|_| self.drag.is_none()) else {
            return;
        };
        let Some(button) = layout
            .hit(point)
            .filter(|button| self.hovered == Some(*button))
        else {
            return;
        };
        if let Some(tooltip) = &mut self.tooltip {
            tooltip.show(layout, button, self.desktop.origin());
        }
    }

    fn hide_tooltip(&mut self) {
        if self.hovered.and_then(Button::tooltip_text).is_some() {
            self.track_mouse(TME_CANCEL | TME_HOVER);
            if let Some(tooltip) = &self.tooltip {
                tooltip.hide();
            }
        }
    }

    fn window_at(&self, point: Point) -> Option<Area> {
        if self.selection.is_some() {
            return None;
        }
        self.windows
            .iter()
            .copied()
            .find(|area| area.contains(point))
    }

    fn cursor_at(&self, point: Option<Point>) -> *const u16 {
        if let Some(drag) = &self.drag {
            return match drag.kind {
                DragKind::Toolbar { .. } => IDC_SIZEALL,
                DragKind::Mark(mark) => mark.cursor(),
                _ => IDC_CROSS,
            };
        }
        if let Some(point) = point {
            if let Some(layout) = self.toolbar_layout() {
                match layout.hit(point) {
                    Some(Button::Grip) => return IDC_SIZEALL,
                    Some(_) => return IDC_HAND,
                    None if layout.contains(point) => return IDC_ARROW,
                    None => {}
                }
            }
            if let Some(crop) = self.selection.filter(|crop| crop.contains(point)) {
                let local = crop.local(point);
                if self.tool == Tool::Text {
                    return IDC_IBEAM;
                }
                if self.tool == Tool::Select {
                    return self.annotations.cursor_at(local, crop.local_bounds());
                }
            }
        }
        IDC_CROSS
    }

    fn set_cursor(&self, point: Option<Point>) {
        unsafe { SetCursor(LoadCursorW(null_mut(), self.cursor_at(point))) };
    }

    fn selection_for_drag(&self, drag: &Drag) -> Option<Area> {
        match drag.kind {
            DragKind::Region { box_started: true } => Area::from_drag(drag.start, drag.end),
            DragKind::Region { box_started: false } => self.window_at(drag.start),
            _ => None,
        }
    }

    fn mouse_up(&mut self, point: Point) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        drag.update(point, self.selection);
        self.commit_drag();
        self.mouse_move(point);
    }

    fn commit_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        unsafe { ReleaseCapture() };
        match drag.kind {
            DragKind::Region { .. } => {
                self.selection = self.selection_for_drag(&drag);
                self.toolbar = self
                    .selection
                    .map(|crop| self.toolbar_origin(crop, drag.end));
            }
            DragKind::Toolbar { origin } => {
                self.toolbar = Some(self.clamp_toolbar(origin + drag.delta(), drag.end));
            }
            DragKind::Pen(stroke) => {
                if stroke.points.len() > 1 {
                    self.annotations.add(Mark::pen(stroke.points, self.color));
                }
            }
            DragKind::Draw(_) => {
                if let Some(mark) = self
                    .selection
                    .and_then(|crop| drag.drawn_mark(crop, self.color))
                {
                    self.annotations.add(mark);
                }
            }
            DragKind::Mark(mark) => {
                if let Some(crop) = self.selection {
                    self.annotations
                        .finish_transform(mark, drag.delta(), crop.local_bounds());
                }
            }
        }
    }

    fn discard_drag(&mut self) {
        if self.drag.take().is_some() {
            unsafe { ReleaseCapture() };
        }
    }

    fn history(&mut self, redo: bool) {
        self.discard_drag();
        if redo {
            self.annotations.redo();
        } else {
            self.annotations.undo();
        }
    }

    fn key_down(&mut self, key: u16) {
        self.apply_pending_click();
        if self.finished.is_some() {
            return;
        }
        let control = unsafe { GetKeyState(i32::from(VK_CONTROL)) < 0 };
        match key {
            VK_ESCAPE if self.palette_open => self.palette_open = false,
            VK_ESCAPE => self.finish(false),
            VK_RETURN => self.finish(true),
            VK_DELETE => {
                if self.annotations.selected().is_some() {
                    self.discard_drag();
                    self.annotations.delete_selected();
                }
            }
            0x5a if control => self.history(unsafe { GetKeyState(i32::from(VK_SHIFT)) < 0 }),
            0x59 if control => self.history(true),
            _ => return,
        }
        self.set_cursor(self.pointer);
    }

    fn toolbar_click(&mut self, button: Button, point: Point) {
        if !matches!(
            button,
            Button::Palette | Button::Swatch(_) | Button::Cancel | Button::Save
        ) {
            if !self.close_text(true) {
                return;
            }
            self.palette_open = false;
        }
        if button == Button::Grip {
            if let Some(origin) = self.toolbar {
                self.begin_drag(point, DragKind::Toolbar { origin });
            }
        } else if matches!(button, Button::Cancel | Button::Undo | Button::Redo) {
            self.defer_click(button);
        } else {
            self.perform_click(button);
        }
    }

    fn perform_click(&mut self, button: Button) {
        match button {
            Button::Tool(tool) => {
                self.tool = tool;
                if tool != Tool::Select {
                    self.annotations.clear_selection();
                }
            }
            Button::Palette => self.palette_open = !self.palette_open,
            Button::Swatch(color) => {
                self.color = color;
                self.palette_open = false;
                self.annotations.recolor_selected(color);
            }
            Button::Undo => self.history(false),
            Button::Redo => self.history(true),
            Button::Cancel => self.finish(false),
            Button::Save => self.finish(true),
            Button::Grip => {}
        }
    }

    fn defer_click(&mut self, button: Button) {
        // A double-click takes priority over a first click that could discard work.
        if unsafe { SetTimer(self.hwnd, CLICK_TIMER, GetDoubleClickTime(), None) } != 0 {
            self.pending_click = Some(button);
        } else {
            self.perform_click(button);
        }
    }

    fn take_pending_click(&mut self) -> Option<Button> {
        let button = self.pending_click.take()?;
        unsafe { KillTimer(self.hwnd, CLICK_TIMER) };
        Some(button)
    }

    fn apply_pending_click(&mut self) {
        if let Some(button) = self.take_pending_click() {
            self.perform_click(button);
        }
    }

    fn finish(&mut self, save: bool) {
        if save {
            if !self.close_text(true) {
                return;
            }
            self.commit_drag();
            if self.selection.is_none() {
                return;
            }
        }
        self.take_pending_click();
        self.hide_tooltip();
        self.finished = Some(save);
        self.text.stop_blink();
        unsafe { PostMessageW(self.owner, WM_CAPTURE_FINISHED, 0, 0) };
    }

    fn close_text(&mut self, keep: bool) -> bool {
        if !self.text.is_open() {
            return true;
        }
        if keep {
            match self.text.commit(self.original.dc) {
                Ok(Some(mark)) => self.annotations.add(Mark::text(mark, self.color)),
                Ok(None) => {}
                Err(_) => return false,
            }
        } else {
            self.text.cancel();
        }
        self.set_cursor(self.pointer);
        true
    }

    fn monitor_at(&self, point: Point) -> Area {
        let origin = Point {
            x: self.desktop.left,
            y: self.desktop.top,
        };
        let monitor = self
            .monitors
            .iter()
            .copied()
            .find(|area| area.contains(point + origin))
            .unwrap_or(self.desktop);
        monitor.offset(Point {
            x: -origin.x,
            y: -origin.y,
        })
    }

    fn toolbar_origin(&self, crop: Area, cursor: Point) -> Point {
        let monitor = self.monitor_at(cursor);
        let spacing = toolbar::HEIGHT + 7;
        let y = if crop.bottom + spacing <= monitor.bottom {
            crop.bottom + 7
        } else if crop.top >= monitor.top + spacing {
            crop.top - spacing
        } else {
            monitor.bottom - spacing
        };
        let x = crop.left + (crop.width() - toolbar::WIDTH) / 2;
        self.clamp_toolbar(Point { x, y }, cursor)
    }

    fn clamp_toolbar(&self, origin: Point, cursor: Point) -> Point {
        let monitor = self.monitor_at(cursor);
        Point {
            x: origin.x.clamp(
                monitor.left,
                (monitor.right - toolbar::WIDTH).max(monitor.left),
            ),
            y: origin.y.clamp(
                monitor.top,
                (monitor.bottom - toolbar::HEIGHT).max(monitor.top),
            ),
        }
    }

    fn toolbar_layout(&self) -> Option<toolbar::Layout> {
        let origin = self.toolbar?;
        Some(toolbar::Layout {
            origin,
            monitor: self.monitor_at(origin),
            palette_open: self.palette_open,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::text::{TextMark, CARET_TIMER};
    use super::*;

    #[test]
    fn selection_covers_both_edges_and_handles_reverse_drag() {
        let start = Point { x: 30, y: 50 };
        let end = Point { x: 10, y: 35 };
        let region = Area::from_drag(start, end).unwrap();
        assert_eq!(Area::from_drag(end, start), Some(region));
        assert_eq!((region.width(), region.height()), (21, 16));
        assert_eq!(region.local(Point { x: 34, y: 27 }), Point { x: 20, y: 0 });
        assert!(region.contains(start) && region.contains(end));
        assert!(Area::from_drag(start, start).is_none());
    }

    fn editor_with_selection() -> Editor {
        let desktop = Area {
            left: -50,
            top: 0,
            right: 640,
            bottom: 160,
        };
        let selection = Area {
            left: 10,
            top: 20,
            right: 130,
            bottom: 100,
        };
        let frame = Bitmap::new(53, 47).unwrap();
        Editor {
            hwnd: null_mut(),
            owner: null_mut(),
            previous: null_mut(),
            desktop,
            monitors: vec![desktop],
            windows: Vec::new(),
            original: Bitmap::new(desktop.width(), desktop.height()).unwrap(),
            antialias: OnceCell::new(),
            text: TextEditor::with_font(Font::new(frame.dc).unwrap()),
            frame: Some(frame),
            selection: Some(selection),
            toolbar: None,
            palette_open: false,
            hovered: None,
            tooltip: None,
            pointer: None,
            pending_click: None,
            drag: None,
            tool: Tool::Rectangle,
            color: INK,
            annotations: Annotations::default(),
            finished: None,
            save_busy: false,
        }
    }

    fn editor_with_overlay() -> Editor {
        use std::sync::Once;
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

        static REGISTER: Once = Once::new();
        let mut editor = editor_with_selection();
        let instance = unsafe { GetModuleHandleW(null()) };
        REGISTER.call_once(|| register(instance).unwrap());
        editor.hwnd = unsafe {
            CreateWindowExW(
                0,
                wide(CLASS).as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                editor.desktop.width(),
                editor.desktop.height(),
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        assert!(!editor.hwnd.is_null());
        editor
    }

    #[test]
    fn click_selects_front_window_and_drag_overrides_it() {
        let mut editor = editor_with_selection();
        let front = editor.selection.take().unwrap();
        let back = Area {
            left: 0,
            top: 0,
            right: 200,
            bottom: 120,
        };
        editor.windows = vec![front, back];
        let start = Point { x: 35, y: 40 };
        let end = Point { x: 150, y: 130 };
        editor.mouse_move(start);
        assert_eq!(editor.window_at(start), Some(front));
        editor.mouse_down(start);
        editor.mouse_up(start);
        assert_eq!(editor.selection, Some(front));
        editor.selection = None;
        editor.toolbar = None;
        editor.mouse_down(start);
        editor.mouse_move(end);
        editor.mouse_up(end);
        assert_eq!(editor.selection, Area::from_drag(start, end));
    }

    #[test]
    fn drawing_keeps_the_tool_active_and_shapes_can_be_selected_and_edited() {
        let mut editor = editor_with_selection();
        let start = Point { x: 30, y: 42 };
        let end = Point { x: 72, y: 75 };
        assert_eq!(editor.tool, Tool::Rectangle);
        editor.mouse_down(start);
        editor.mouse_up(end);
        let original = editor.annotations.marks()[0].bounds();
        assert_eq!(editor.tool, Tool::Rectangle);
        editor.mouse_down(Point { x: 95, y: 45 });
        editor.mouse_up(Point { x: 120, y: 75 });
        assert_eq!(editor.annotations.marks().len(), 2);
        assert_eq!(editor.annotations.selected().map(Mark::bounds), None);

        editor.perform_click(Button::Tool(Tool::Select));
        let center = Point { x: 55, y: 58 };
        editor.mouse_down(center);
        editor.mouse_up(center);
        assert_eq!(
            editor.annotations.selected().map(Mark::bounds),
            Some(original)
        );

        let east = Point { x: 72, y: 58 };
        assert_eq!(editor.cursor_at(Some(east)), IDC_SIZEWE);
        editor.mouse_down(east);
        editor.mouse_up(east + Point { x: 12, y: 0 });
        let resized = editor.annotations.marks()[0].bounds();
        assert_eq!(resized.width(), original.width() + 12);
        assert_eq!(resized.height(), original.height());
        let delta = Point { x: 7, y: 9 };
        editor.mouse_down(center);
        editor.mouse_up(center + delta);
        assert_eq!(
            editor.annotations.marks()[0].bounds(),
            resized.offset(delta)
        );
        editor.history(false);
        assert_eq!(editor.annotations.marks()[0].bounds(), resized);
        editor.history(true);
        assert_eq!(
            editor.annotations.marks()[0].bounds(),
            resized.offset(delta)
        );

        let mut editor = editor_with_selection();
        editor.tool = Tool::Arrow;
        editor.mouse_down(start);
        editor.mouse_up(end);
        assert_eq!(editor.tool, Tool::Arrow);
        editor.perform_click(Button::Tool(Tool::Select));
        editor.mouse_down(start);
        editor.mouse_up(start);
        editor.mouse_down(end);
        editor.mouse_up(end + Point { x: 10, y: -12 });
        let changed = editor.annotations.marks()[0].bounds();
        assert_eq!(changed.right, 73);
        assert_eq!(changed.bottom, 44);
        let middle = Point { x: 56, y: 53 };
        editor.mouse_down(middle);
        editor.mouse_up(middle + delta);
        assert_eq!(
            editor.annotations.marks()[0].bounds(),
            changed.offset(delta)
        );

        let mut editor = editor_with_selection();
        editor.tool = Tool::Pen;
        editor.mouse_down(start);
        let bend = Point { x: 50, y: 53 };
        editor.mouse_move(bend);
        editor.mouse_up(end);
        assert_eq!(editor.tool, Tool::Pen);
        editor.perform_click(Button::Tool(Tool::Select));
        editor.mouse_down(bend);
        editor.mouse_up(bend);
        let original = editor.annotations.marks()[0].bounds();
        let south = Point { x: 51, y: 75 };
        assert_eq!(editor.cursor_at(Some(south)), IDC_SIZENS);
        editor.mouse_down(south);
        editor.mouse_up(south + Point { x: 0, y: 10 });
        assert_eq!(
            editor.annotations.marks()[0].bounds().height(),
            original.height() + 10
        );
        let ink = rgb(17, 120, 250);
        editor.perform_click(Button::Swatch(ink));
        assert_eq!(editor.annotations.marks()[0].color, ink);
        editor.key_down(VK_DELETE);
        assert!(editor.annotations.marks().is_empty());
        editor.history(false);
        assert_eq!(editor.annotations.marks()[0].color, ink);
    }

    #[test]
    fn preview_and_export_render_annotations_at_crop_coordinates() {
        let mut editor = editor_with_selection();
        let selection = editor.selection.unwrap();
        let desktop = editor.desktop;
        let background = [30, 90, 140];
        for pixel in editor.original.bytes_mut().chunks_exact_mut(4) {
            pixel.copy_from_slice(&[background[2], background[1], background[0], 0]);
        }
        for mark in [
            Mark::rectangle(
                Area {
                    left: 8,
                    top: 8,
                    right: 40,
                    bottom: 35,
                },
                INK,
            ),
            Mark::arrow(Point { x: 52, y: 10 }, Point { x: 90, y: 10 }, INK),
            Mark::text(
                TextMark::new(
                    editor.original.dc,
                    editor.text.font().unwrap(),
                    selection,
                    Point { x: 8, y: 45 },
                    "中文".encode_utf16().collect(),
                ),
                INK,
            ),
        ] {
            editor.annotations.add(mark);
        }
        let image = editor.render_annotated_image(selection).unwrap();
        assert_eq!((image.width, image.height), (120, 80));
        let rgb = image.rgb();
        let pixel = |x: usize, y: usize| &rgb[(y * 120 + x) * 3..][..3];
        assert_eq!(pixel(0, 0), background);
        assert_eq!(pixel(22, 8), [231, 43, 52]);
        assert_eq!(pixel(22, 20), background);
        assert_eq!(pixel(70, 10), [231, 43, 52]);
        assert_eq!(pixel(9, 46), background);
        assert!((11..54).any(|x| (47..69).any(|y| {
            let p = pixel(x, y);
            p[0] > p[1] && p[0] > p[2]
        })));
        let preview = Bitmap::new(desktop.width(), desktop.height()).unwrap();
        editor.paint(preview.dc, &[desktop.local_bounds().rect()]);
        unsafe {
            GdiFlush();
        }
        let preview_pixel = |x, y| {
            let index = ((selection.top + y) * desktop.width() + selection.left + x) as usize * 4;
            &preview.bytes()[index..index + 3]
        };
        assert_eq!(preview_pixel(0, 0), [134, 220, 72]);
        assert_eq!(preview_pixel(22, 8), [52, 43, 231]);
        assert_eq!(preview_pixel(9, 46), [140, 90, 30]);

        let grab = Point {
            x: selection.left + 10,
            y: selection.top + 47,
        };
        editor.tool = Tool::Select;
        assert_eq!(editor.cursor_at(Some(grab)), IDC_SIZEALL);
        let drop = Point {
            x: selection.right + 10,
            y: selection.bottom + 10,
        };
        editor.mouse_down(grab);
        editor.mouse_move(drop);
        editor.mouse_up(drop);
        let moved = &editor.annotations.marks()[2];
        assert_eq!(moved.bounds().right, selection.width());
        assert_eq!(moved.bounds().bottom, selection.height());
        assert_eq!(editor.cursor_at(Some(grab)), IDC_ARROW);
        let image = editor.render_annotated_image(selection).unwrap();
        let old_pixel = (55 * image.width as usize + 11) * 3;
        let image = image.rgb();
        assert_eq!(image[old_pixel..old_pixel + 3], background);

        let corner =
            moved.controls_bounds(selection.local_bounds()).origin() + Point { x: 4, y: 4 };
        let corner = corner + selection.origin();
        editor.mouse_down(corner);
        editor.mouse_up(corner + Point { x: -16, y: -10 });
        let bounds = editor.annotations.marks()[2].bounds();
        let scaled_image = editor.render_annotated_image(selection).unwrap().rgb();
        let red_pixels = |image: &[u8]| {
            (40..80)
                .flat_map(|y| (60..120).map(move |x| (x, y)))
                .filter(|&(x, y)| image[(y * 120 + x) * 3] > 180)
                .count()
        };
        assert!(red_pixels(&scaled_image) > red_pixels(&image));
        let control_x = bounds.left as usize;
        let control_y = bounds.top as usize;
        let exported = (control_y * 120 + control_x) * 3;
        assert_eq!(scaled_image[exported..exported + 3], background);
    }

    #[test]
    fn annotation_handles_contrast_with_light_and_dark_backgrounds() {
        let editor = editor_with_selection();
        let crop = editor.selection.unwrap().local_bounds();
        let start = Point { x: 20, y: 15 };
        let end = Point { x: 85, y: 45 };
        let marks = [
            Mark::rectangle(Area::from_drag(start, end).unwrap(), INK),
            Mark::arrow(start, end, INK),
            Mark::pen(vec![start, end], INK),
            Mark::text(
                TextMark::new(
                    editor.original.dc,
                    editor.text.font().unwrap(),
                    crop,
                    start,
                    "标注".encode_utf16().collect(),
                ),
                INK,
            ),
        ];
        for mark in marks {
            let frame = Bitmap::new(crop.width(), crop.height()).unwrap();
            let corner = mark.controls_bounds(crop).origin();
            for background in [0, crate::appearance::WHITE] {
                unsafe {
                    crate::appearance::fill(frame.dc, &crop.rect(), background);
                    mark.paint_controls(frame.dc, crop);
                    let outline = GetPixel(frame.dc, corner.x, corner.y);
                    assert_ne!(outline, crate::appearance::WHITE);
                    assert_ne!(outline, background);
                    assert_eq!(
                        GetPixel(frame.dc, corner.x + 4, corner.y + 4),
                        crate::appearance::WHITE
                    );
                }
            }
        }
    }

    #[test]
    fn diagonal_strokes_blend_edges_in_preview_and_export() {
        let mut editor = editor_with_selection();
        assert!(editor.antialias().is_some());
        let crop = editor.selection.unwrap();
        let background = [20, 60, 160];
        for pixel in editor.original.bytes_mut().chunks_exact_mut(4) {
            pixel.copy_from_slice(&[background[2], background[1], background[0], 0]);
        }
        for mark in [
            Mark::arrow(Point { x: 0, y: 1 }, Point { x: 52, y: 33 }, INK),
            Mark::pen(
                vec![
                    Point { x: 62, y: 49 },
                    Point { x: 84, y: 65 },
                    Point {
                        x: crop.width() - 1,
                        y: 53,
                    },
                ],
                INK,
            ),
        ] {
            editor.annotations.add(mark);
        }
        let image = editor.render_annotated_image(crop).unwrap();
        let rgb = image.rgb();
        let blended = |from, to| {
            rgb.chunks_exact(3).enumerate().any(|(index, pixel)| {
                let x = index % image.width as usize;
                (from..to).contains(&x)
                    && (background[0] + 1..231).contains(&pixel[0])
                    && (53..background[2]).contains(&pixel[2])
            })
        };
        assert!(blended(0, 53));
        assert!(blended(60, 120));

        let frame = Bitmap::new(editor.desktop.width(), editor.desktop.height()).unwrap();
        editor.paint(frame.dc, &[editor.desktop.local_bounds().rect()]);
        unsafe { GdiFlush() };
        let preview = |x, y| {
            let index = (y * editor.desktop.width() + x) as usize * 4;
            &frame.bytes()[index..index + 3]
        };
        for y in 1..crop.height() - 1 {
            for x in 1..crop.width() - 1 {
                let index = (y * crop.width() + x) as usize * 3;
                let [r, g, b] = rgb[index..index + 3] else {
                    unreachable!()
                };
                assert_eq!(preview(crop.left + x, crop.top + y), [b, g, r]);
            }
        }
        let shaded = preview(crop.left - 1, crop.top + 1);
        for (&channel, original) in shaded.iter().zip([160, 60, 20]) {
            assert!((i32::from(channel) - original * 43 / 100).abs() <= 1);
        }
    }

    #[test]
    fn confirming_during_a_drag_exports_the_visible_mark_and_transform() {
        let drawing_start = Point { x: 30, y: 42 };
        let drawing_end = Point { x: 72, y: 75 };
        let move_start = Point { x: 52, y: 59 };
        let move_end = move_start + Point { x: 10, y: 9 };
        let outline = |release: bool, transform: bool| {
            let mut editor = editor_with_selection();
            editor.mouse_down(drawing_start);
            editor.mouse_move(drawing_end);
            if transform {
                editor.mouse_up(drawing_end);
                editor.perform_click(Button::Tool(Tool::Select));
                editor.mouse_down(move_start);
                editor.mouse_move(move_end);
            }
            if release {
                editor.mouse_up(if transform { move_end } else { drawing_end });
            }
            editor.key_down(VK_RETURN);
            assert_eq!(editor.finished, Some(true));
            editor.export(usize::MAX).unwrap().rgb()
        };
        let drawn = outline(false, false);
        let moved = outline(false, true);
        assert!(drawn.chunks_exact(3).any(|pixel| pixel == [231, 43, 52]));
        assert_ne!(drawn, moved);
        assert_eq!(drawn, outline(true, false));
        assert_eq!(moved, outline(true, true));
    }

    #[test]
    fn exports_cropped_rows_after_releasing_the_desktop_bitmap() {
        for annotated in [false, true] {
            let mut editor = editor_with_selection();
            let desktop = editor.desktop.local_bounds();
            let crop = Area {
                left: 17,
                top: 13,
                ..desktop
            };
            editor.selection = Some(crop);
            let stride = desktop.width() as usize;
            for (index, pixel) in editor.original.bytes_mut().chunks_exact_mut(4).enumerate() {
                pixel.copy_from_slice(&[(index % stride) as u8, (index / stride) as u8, 121, 0]);
            }
            if annotated {
                editor.annotations.add(Mark::arrow(
                    Point { x: 0, y: 0 },
                    Point { x: 31, y: 21 },
                    INK,
                ));
            }
            let tight = editor.render_annotated_image(crop).unwrap().rgb();
            let image = editor.export(usize::MAX).unwrap();
            assert_eq!(
                image.byte_len(),
                desktop.width() as usize * desktop.height() as usize * 4
            );
            assert_eq!(image.height, crop.height() as u32);
            assert_eq!(image.width, crop.width() as u32);
            let rgb = image.rgb();
            assert_eq!(
                &rgb[rgb.len() - 3..],
                &[121, (desktop.bottom - 1) as u8, (desktop.right - 1) as u8]
            );
            assert_eq!(rgb, tight);
        }
        let editor = editor_with_selection();
        let crop = editor.selection.unwrap();
        let length = crop.width() as usize * crop.height() as usize * 4;
        assert!(editor.exceeds_save_budget(length - 1));
        assert_eq!(editor.export(length).unwrap().byte_len(), length);
    }

    #[test]
    fn busy_save_keeps_the_selection_and_annotations_available() {
        let mut editor = editor_with_selection();
        editor.annotations.add(Mark::arrow(
            Point { x: 3, y: 5 },
            Point { x: 28, y: 10 },
            INK,
        ));
        editor.finish(true);
        EDITOR.with(|slot| slot.replace(Some(editor)));
        assert!(complete(0).unwrap().is_none());
        with_editor(|editor| {
            assert!(editor.save_busy);
            assert_eq!(editor.annotations.marks().len(), 1);
            editor.finish(true);
        });
        assert!(complete(usize::MAX).unwrap().is_some());
    }

    #[test]
    fn live_pen_damage_matches_full_render_for_a_long_crossing_stroke() {
        let mut editor = editor_with_selection();
        let desktop = editor.desktop.local_bounds();
        let crop = editor.selection.unwrap();
        editor.tool = Tool::Pen;
        let partial = Bitmap::new(desktop.width(), desktop.height()).unwrap();
        editor.paint(partial.dc, &[desktop.rect()]);

        let points = (4..112)
            .map(|x| Point {
                x,
                y: 14 + x * 3 % 57,
            })
            .chain((4..112).rev().map(|x| Point {
                x,
                y: 10 + x * 5 % 57,
            }))
            .collect::<Vec<_>>();
        editor.mouse_down(crop.origin() + points[0]);
        for &point in points.iter().skip(1) {
            let before = editor.preview_snapshot();
            editor.mouse_move(crop.origin() + point);
            let mut dirty = Vec::new();
            editor.repaint_changes(before, |area| dirty.push(area));
            assert_eq!(dirty.len(), 1);
            assert!(dirty[0].width() <= 8);
            editor.paint(partial.dc, &[dirty[0].rect()]);
        }
        editor.discard_drag();
        editor.annotations.add(Mark::pen(points, editor.color));
        editor.frame = Some(Bitmap::new(desktop.width(), desktop.height()).unwrap());
        let full = Bitmap::new(desktop.width(), desktop.height()).unwrap();
        editor.paint(full.dc, &[desktop.rect()]);
        unsafe { GdiFlush() };
        assert!(partial
            .bytes()
            .chunks_exact(4)
            .zip(full.bytes().chunks_exact(4))
            .all(|(actual, expected)| actual[..3] == expected[..3]));
    }

    #[test]
    fn partial_repaint_matches_full_repaint_across_strokes_and_toolbar() {
        let mut editor = editor_with_selection();
        let desktop = editor.desktop.local_bounds();
        editor.toolbar = Some(Point { x: 20, y: 102 });
        editor.palette_open = true;
        editor.annotations.add(Mark::arrow(
            Point { x: 4, y: 15 },
            Point { x: 102, y: 60 },
            INK,
        ));
        for pixel in editor.original.bytes_mut().chunks_exact_mut(4) {
            pixel.copy_from_slice(&[140, 90, 30, 0]);
        }
        let split = 54;
        let full = Bitmap::new(desktop.width(), desktop.height()).unwrap();
        editor.frame = Some(Bitmap::new(desktop.width(), desktop.height()).unwrap());
        editor.paint(full.dc, &[desktop.rect()]);
        unsafe { GdiFlush() };
        let partial = Bitmap::new(desktop.width(), desktop.height()).unwrap();
        editor.frame = Some(Bitmap::new(53, 47).unwrap());
        editor.paint(
            partial.dc,
            &[Area {
                right: split,
                ..desktop
            }
            .rect()],
        );
        unsafe { GdiFlush() };
        let column = (15 * desktop.width() + split) as usize * 4;
        assert_eq!(&partial.bytes()[column..column + 3], &[0, 0, 0]);
        editor.paint(
            partial.dc,
            &[Area {
                left: split,
                ..desktop
            }
            .rect()],
        );
        unsafe { GdiFlush() };
        assert!(partial
            .bytes()
            .chunks_exact(4)
            .zip(full.bytes().chunks_exact(4))
            .all(|(actual, expected)| actual[..3] == expected[..3]));
    }

    #[test]
    fn preview_damage_matches_a_full_repaint_through_transforms_and_text_changes() {
        fn check_update(
            editor: &mut Editor,
            incremental: &Bitmap,
            full: &Bitmap,
            action: impl FnOnce(&mut Editor),
        ) -> Vec<Area> {
            let before = editor.preview_snapshot();
            action(editor);
            let mut damage = Vec::new();
            editor.repaint_changes(before, |area| damage.push(area));
            assert!(!damage.is_empty());
            let rectangles: Vec<_> = damage.iter().map(|area| area.rect()).collect();
            editor.paint(incremental.dc, &rectangles);
            editor.paint(full.dc, &[editor.desktop.local_bounds().rect()]);
            unsafe { GdiFlush() };
            assert!(incremental
                .bytes()
                .chunks_exact(4)
                .zip(full.bytes().chunks_exact(4))
                .all(|(actual, expected)| actual[..3] == expected[..3]));
            damage
        }

        let mut editor = editor_with_overlay();
        let desktop = editor.desktop;
        let crop = editor.selection.unwrap();
        let incremental = Bitmap::new(desktop.width(), desktop.height()).unwrap();
        let full = Bitmap::new(desktop.width(), desktop.height()).unwrap();
        editor.paint(incremental.dc, &[desktop.local_bounds().rect()]);
        let mark_start = Point { x: 30, y: 37 };
        let mark_end = Point { x: 58, y: 58 };
        check_update(&mut editor, &incremental, &full, |editor| {
            editor.mouse_down(mark_start);
            editor.mouse_up(mark_end);
            editor.perform_click(Button::Tool(Tool::Select));
        });
        let grab = Point { x: 40, y: 37 };
        let moved = grab + Point { x: 42, y: 22 };
        check_update(&mut editor, &incremental, &full, |editor| {
            editor.mouse_down(grab)
        });
        check_update(&mut editor, &incremental, &full, |editor| {
            editor.mouse_move(moved)
        });
        check_update(&mut editor, &incremental, &full, |editor| {
            editor.mouse_up(moved)
        });
        check_update(&mut editor, &incremental, &full, |editor| {
            editor.perform_click(Button::Swatch(rgb(80, 120, 200)));
        });

        check_update(&mut editor, &incremental, &full, |editor| {
            editor.perform_click(Button::Tool(Tool::Text));
            editor
                .text
                .open(editor.hwnd, editor.original.dc, crop, Point { x: 9, y: 20 });
            let source = editor.text.hwnd();
            unsafe { SetWindowTextW(source, wide("中文").as_ptr()) };
            editor.text.refresh(editor.original.dc, source);
        });
        let draft = editor.text.preview().unwrap().1.bounds();
        check_update(&mut editor, &incremental, &full, |editor| {
            use windows_sys::Win32::UI::Controls::EM_SETSEL;
            let source = editor.text.hwnd();
            unsafe { SendMessageW(source, EM_SETSEL, 0, 1) };
            editor.text.refresh(editor.original.dc, source);
        });
        assert_eq!(editor.text.preview().unwrap().1.bounds(), draft);

        let damage = check_update(&mut editor, &incremental, &full, |editor| {
            editor.text.blink()
        });
        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0].width(), 1);
        assert!(editor.close_text(false));
        unsafe { DestroyWindow(editor.hwnd) };
    }

    #[test]
    fn text_padding_yields_to_text_that_fits_on_one_line() {
        let mut editor = editor_with_overlay();
        let font = editor.text.font().unwrap();
        let units = ['中' as u16; 2];
        let width = unsafe {
            let dc = editor.original.dc;
            let previous = SelectObject(dc, font.handle);
            let width = text::measure(dc, &units);
            SelectObject(dc, previous);
            width
        };
        let mut crop = editor.selection.unwrap();
        crop.bottom = crop.top + font.line_height;
        for (available, fits) in [(width, true), (width + 4, true), (width - 1, false)] {
            crop.right = crop.left + available;
            editor.selection = Some(crop);
            editor
                .text
                .open(editor.hwnd, editor.original.dc, crop, Point { x: 0, y: 0 });
            unsafe { SetWindowTextW(editor.text.hwnd(), wide("中中").as_ptr()) };
            editor.text.refresh(editor.original.dc, editor.text.hwnd());
            assert_eq!(editor.text.preview().unwrap().1.overflows(), !fits);
            assert_eq!(editor.close_text(true), fits);
            if fits {
                assert_eq!(
                    editor.annotations.marks().last().unwrap().bounds().width(),
                    width
                );
            } else {
                assert!(editor.close_text(false));
            }
        }
        assert_eq!(editor.annotations.marks().len(), 2);
        unsafe { DestroyWindow(editor.hwnd) };
    }

    #[test]
    fn text_caret_blinks_inside_the_selection_outline() {
        let mut editor = editor_with_overlay();
        let font = editor.text.font().unwrap();
        let mut crop = editor.selection.unwrap();
        crop.right = crop.left + font.chinese_width + 1;
        crop.bottom = crop.top + font.line_height;
        editor.selection = Some(crop);
        editor
            .text
            .open(editor.hwnd, editor.original.dc, crop, Point { x: 0, y: 0 });
        unsafe { SendMessageW(editor.text.hwnd(), WM_CHAR, '中' as WPARAM, 1) };
        editor.text.refresh(editor.original.dc, editor.text.hwnd());
        let caret = editor.text.preview().unwrap().1.caret_bounds();
        let point = caret.origin() + crop.origin();
        let preview = Bitmap::new(crop.right, crop.bottom).unwrap();
        editor.paint(preview.dc, &[crop.rect()]);
        assert_eq!(
            unsafe { GetPixel(preview.dc, point.x, point.y) },
            SELECTION_BORDER
        );
        editor.text.blink();
        editor.paint(preview.dc, &[Area::pixel(point).rect()]);
        assert_ne!(
            unsafe { GetPixel(preview.dc, point.x, point.y) },
            SELECTION_BORDER
        );
        assert_eq!(
            caret.intersection(crop.local_bounds().padded(-1)),
            Some(caret)
        );
        assert!(editor.close_text(false));
        unsafe { DestroyWindow(editor.hwnd) };
    }

    #[test]
    fn double_click_finishes_on_canvas_toolbar_and_text_input() {
        let mut editor = editor_with_overlay();
        let hwnd = editor.hwnd;
        editor.toolbar = Some(Point { x: 10, y: 107 });
        let selection = editor.selection.unwrap();
        let anchor = Point {
            x: selection.width() - 2 * editor.text.font().unwrap().chinese_width - 16,
            y: 20,
        };
        editor.annotations.add(Mark::text(
            TextMark::new(
                editor.original.dc,
                editor.text.font().unwrap(),
                selection,
                Point { x: 0, y: 0 },
                "test".encode_utf16().collect(),
            ),
            INK,
        ));
        let button_point = |target| {
            let (_, area) = editor
                .toolbar_layout()
                .unwrap()
                .buttons()
                .find(|(button, _)| *button == target)
                .unwrap();
            Point {
                x: area.left + 1,
                y: area.top + 1,
            }
        };
        let cancel = button_point(Button::Cancel);
        let undo = button_point(Button::Undo);
        let send_mouse = |message, point: Point| unsafe {
            SendMessageW(hwnd, message, 0, ((point.y << 16) | point.x) as LPARAM);
        };
        let send_edit_key = |hwnd, key: u16| {
            assert!(handle_edit_key(&MSG {
                hwnd,
                message: WM_KEYDOWN,
                wParam: usize::from(key),
                ..MSG::default()
            }));
        };
        EDITOR.with(|slot| slot.replace(Some(editor)));

        let outside = Point { x: 0, y: 0 };
        with_editor(|editor| {
            let selection = editor.selection.take();
            editor.finish(true);
            assert_eq!(editor.finished, None);
            editor.selection = selection;
        });
        let check_saved = |mark_count, tool| {
            EDITOR.with(|slot| {
                let mut slot = slot.borrow_mut();
                let editor = slot.as_mut().unwrap();
                assert_eq!(editor.finished.take(), Some(true));
                assert_eq!(editor.annotations.marks().len(), mark_count);
                assert_eq!(editor.tool, tool);
            });
        };
        for point in [outside, Point { x: 31, y: 35 }, cancel, undo] {
            send_mouse(WM_LBUTTONDOWN, point);
            send_mouse(WM_LBUTTONUP, point);
            send_mouse(WM_LBUTTONDBLCLK, point);
            check_saved(1, Tool::Rectangle);
        }

        let mut edit = null_mut();
        let mut caret = selection.origin();
        with_editor(|editor| {
            editor.tool = Tool::Text;
            editor
                .text
                .open(editor.hwnd, editor.original.dc, selection, anchor);
            let input = editor.text.preview().unwrap().1;
            edit = editor.text.hwnd();
            assert!(input.bounds().width() > editor.text.font().unwrap().chinese_width);
            let bar = input.caret_bounds();
            assert_eq!(input.bounds().padded(-3).intersection(bar), Some(bar));
            caret = caret + bar.origin();
        });
        let preview = Bitmap::new(selection.right, selection.bottom).unwrap();
        let caret_color = || {
            with_editor(|editor| editor.paint(preview.dc, &[Area::pixel(caret).rect()]));
            unsafe { GetPixel(preview.dc, caret.x, caret.y) }
        };
        assert_eq!(caret_color(), SELECTION_BORDER);
        unsafe { SendMessageW(hwnd, WM_TIMER, CARET_TIMER, 0) };
        assert_eq!(caret_color(), 0);
        unsafe { SendMessageW(hwnd, WM_TIMER, CARET_TIMER, 0) };
        assert_eq!(caret_color(), SELECTION_BORDER);
        unsafe {
            SendMessageW(hwnd, WM_TIMER, CARET_TIMER, 0);
            SendMessageW(hwnd, text::UPDATED, edit as WPARAM, 0);
        }
        assert_eq!(caret_color(), SELECTION_BORDER);
        unsafe {
            for unit in "中文标注".encode_utf16() {
                SendMessageW(edit, WM_CHAR, unit as WPARAM, 1);
            }
            let mut updated = MSG::default();
            assert_ne!(
                PeekMessageW(&mut updated, hwnd, text::UPDATED, text::UPDATED, PM_REMOVE),
                0
            );
            DispatchMessageW(&updated);
        }
        with_editor(|editor| {
            let input = editor.text.preview().unwrap().1;
            let bounds = input.bounds();
            let bar = input.caret_bounds();
            let font = editor.text.font().unwrap();
            assert!(bounds.width() > font.chinese_width);
            assert!(bounds.right <= selection.width());
            assert!(bounds.height() <= selection.height());
            assert!(bounds.height() > font.line_height);
            assert_eq!(bounds.padded(-3).intersection(bar), Some(bar));
            assert_eq!(
                crate::control_text(edit),
                "中文标注".encode_utf16().collect::<Vec<_>>()
            );
        });
        let draft_point = Point {
            x: selection.left + anchor.x + 1,
            y: selection.top + anchor.y + 1,
        };
        send_mouse(WM_LBUTTONDOWN, draft_point);
        send_mouse(WM_LBUTTONUP, draft_point);
        send_mouse(WM_LBUTTONDBLCLK, draft_point);
        check_saved(2, Tool::Text);

        with_editor(|editor| {
            editor.tool = Tool::Text;
            editor
                .text
                .open(editor.hwnd, editor.original.dc, selection, anchor);
            edit = editor.text.hwnd();
        });
        unsafe { SetWindowTextW(edit, wide("\r\n\r\n\r\n\r\n\r\n文字").as_ptr()) };
        send_edit_key(edit, VK_RETURN);
        send_mouse(WM_LBUTTONDBLCLK, draft_point);
        with_editor(|editor| {
            assert_eq!(editor.finished, None);
            assert_eq!(editor.annotations.marks().len(), 2);
            assert!(editor.text.preview().unwrap().1.overflows());
        });
        unsafe { SetWindowTextW(edit, wide("文字").as_ptr()) };
        send_edit_key(edit, VK_RETURN);
        with_editor(|editor| {
            assert!(!editor.text.is_open());
            assert_eq!(editor.annotations.marks().len(), 3);
            assert_eq!(editor.tool, Tool::Text);
        });

        let next_anchor = Point {
            x: selection.right - 2,
            y: selection.bottom - 2,
        };
        send_mouse(WM_LBUTTONDOWN, next_anchor);
        with_editor(|editor| {
            let input = editor.text.preview().unwrap().1;
            edit = editor.text.hwnd();
            let bounds = input.bounds();
            let bar = input.caret_bounds();
            assert_eq!(bounds.intersection(selection.local_bounds()), Some(bounds));
            assert_eq!(bounds.right, selection.width());
            assert_eq!(bounds.bottom, selection.height());
            assert_eq!(bounds.padded(-3).intersection(bar), Some(bar));
        });
        send_edit_key(edit, VK_ESCAPE);

        send_mouse(WM_LBUTTONDOWN, cancel);
        send_mouse(WM_LBUTTONUP, cancel);
        unsafe { SendMessageW(hwnd, WM_TIMER, CLICK_TIMER, 0) };
        assert!(complete(usize::MAX).unwrap().is_none());
    }
}
