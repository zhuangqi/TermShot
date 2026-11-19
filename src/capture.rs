use crate::appearance::{self, rgb};
use crate::storage::wide;
use crate::{register_class, WM_CAPTURE_FINISHED};
use std::cell::RefCell;
use std::io;
use std::ops::{Add, Sub};
use std::ptr::{null, null_mut};
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::Input::Ime::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

mod annotations;
mod strokes;
mod text;
mod toolbar;
mod windows;
use annotations::{Change, Handle, Mark, Tool};
use text::{TextInput, TextMark};
use toolbar::Button;

const CLASS: &str = "CliScreenshot.Overlay";
const TEXT_FINISHED: u32 = WM_APP + 10;
const CLICK_TIMER: usize = 1;
const INK: u32 = rgb(231, 43, 52);
const ANNOTATION_FONT_HEIGHT: i32 = 24;

thread_local! {
    static EDITOR: RefCell<Option<Editor>> = const { RefCell::new(None) };
}

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
    Region {
        box_started: bool,
    },
    Draw {
        tool: Tool,
        points: Vec<Point>,
    },
    Mark {
        index: usize,
        handle: Option<Handle>,
    },
    Toolbar {
        origin: Point,
    },
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
            DragKind::Draw {
                tool: Tool::Pen,
                points,
            } => {
                if let Some(crop) = crop {
                    let point = crop.local(end);
                    if points.last() != Some(&point) {
                        points.push(point);
                    }
                }
            }
            _ => {}
        }
    }

    fn delta(&self) -> Point {
        self.end - self.start
    }

    fn drawn_mark(&self, crop: Area, color: COLORREF) -> Option<Mark> {
        let DragKind::Draw { tool, points } = &self.kind else {
            return None;
        };
        let start = crop.local(self.start);
        let end = crop.local(self.end);
        match tool {
            Tool::Rectangle => Area::from_drag(start, end).map(|area| Mark::rectangle(area, color)),
            Tool::Arrow if start != end => Some(Mark::arrow(start, end, color)),
            Tool::Pen if points.len() > 1 => Some(Mark::pen(points.clone(), color)),
            _ => None,
        }
    }
}

enum ClickAction {
    Button(Button),
    Erase(usize),
}

struct Editor {
    hwnd: HWND,
    owner: HWND,
    previous: HWND,
    desktop: Area,
    monitors: Vec<Area>,
    windows: Vec<Area>,
    original: Bitmap,
    shaded: Bitmap,
    frame: Bitmap,
    font: Font,
    antialias: Option<strokes::Antialias>,
    selection: Option<Area>,
    toolbar: Option<Point>,
    palette_open: bool,
    hovered: Option<Button>,
    pointer: Option<Point>,
    pending_click: Option<ClickAction>,
    drag: Option<Drag>,
    tool: Tool,
    color: COLORREF,
    marks: Vec<Mark>,
    selected_mark: Option<usize>,
    undo: Vec<Change>,
    redo: Vec<Change>,
    text: Option<TextInput>,
    finished: Option<bool>,
}

pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

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
        let mut shaded = Bitmap::new(desktop.width(), desktop.height())?;
        let frame = Bitmap::new(desktop.width(), desktop.height())?;
        for (target, source) in shaded
            .bytes_mut()
            .chunks_exact_mut(4)
            .zip(original.bytes().chunks_exact(4))
        {
            for channel in 0..3 {
                target[channel] = (u16::from(source[channel]) * 43 / 100) as u8;
            }
        }
        let font = Font::new(frame.dc)?;
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            wide(CLASS).as_ptr(),
            wide("CliScreenshot 截图").as_ptr(),
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
                shaded,
                frame,
                font,
                antialias: strokes::Antialias::new(),
                selection: None,
                toolbar: None,
                palette_open: false,
                hovered: None,
                pointer: None,
                pending_click: None,
                drag: None,
                tool: Tool::Rectangle,
                color: INK,
                marks: Vec::new(),
                selected_mark: None,
                undo: Vec::new(),
                redo: Vec::new(),
                text: None,
                finished: None,
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
            with_editor(|editor| editor.mouse_move(point));
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

pub fn complete() -> io::Result<Option<Image>> {
    let Some(editor) = EDITOR.with(|slot| slot.take()) else {
        return Ok(None);
    };
    let save = editor.finished == Some(true);
    unsafe {
        let was_foreground = GetForegroundWindow() == editor.hwnd;
        DestroyWindow(editor.hwnd);
        if was_foreground && !editor.previous.is_null() && IsWindow(editor.previous) != 0 {
            SetForegroundWindow(editor.previous);
        }
    }
    if save {
        editor.export().map(Some)
    } else {
        Ok(None)
    }
}

pub fn handle_edit_key(message: &MSG) -> bool {
    if message.message != WM_KEYDOWN || !matches!(message.wParam as u16, VK_RETURN | VK_ESCAPE) {
        return false;
    }
    let overlay = EDITOR.with(|slot| {
        let state = slot.borrow();
        let editor = state.as_ref()?;
        (editor.text.as_ref()?.hwnd == message.hwnd).then_some(editor.hwnd)
    });
    let Some(overlay) = overlay else { return false };
    unsafe {
        // Let the native edit control finish an IME composition before interpreting Enter or Escape.
        let context = ImmGetContext(message.hwnd);
        if !context.is_null() {
            let composing = ImmGetCompositionStringW(context, GCS_COMPSTR, null_mut(), 0) > 0;
            ImmReleaseContext(message.hwnd, context);
            if composing {
                return false;
            }
        }
        SendMessageW(
            overlay,
            TEXT_FINISHED,
            usize::from(message.wParam as u16 == VK_RETURN),
            0,
        );
    }
    true
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_PAINT {
        let mut paint = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut paint);
        with_editor(|editor| editor.paint(dc));
        EndPaint(hwnd, &paint);
        return 0;
    }
    match message {
        WM_ERASEBKGND => return 1,
        WM_LBUTTONDOWN => with_editor(|editor| editor.mouse_down(editor.pointer(lparam))),
        WM_MOUSEMOVE => with_editor(|editor| editor.mouse_move(editor.pointer(lparam))),
        WM_LBUTTONUP => with_editor(|editor| editor.mouse_up(editor.pointer(lparam))),
        WM_LBUTTONDBLCLK => with_editor(|editor| editor.finish(true)),
        WM_TIMER if wparam == CLICK_TIMER => with_editor(Editor::apply_pending_click),
        WM_MOUSELEAVE => with_editor(|editor| {
            editor.pointer = None;
            editor.hovered = None;
            editor.repaint();
        }),
        WM_SETCURSOR if wparam as HWND == hwnd => {
            with_editor(|editor| editor.set_cursor(windows::pointer(hwnd)));
            return 1;
        }
        WM_CAPTURECHANGED => with_editor(|editor| {
            if editor.drag.take().is_some() {
                InvalidateRect(hwnd, null(), 0);
            }
        }),
        WM_KEYDOWN => with_editor(|editor| editor.key_down(wparam as u16)),
        WM_IME_SETCONTEXT => {
            return DefWindowProcW(
                hwnd,
                message,
                wparam,
                lparam & !(ISC_SHOWUICOMPOSITIONWINDOW as isize),
            );
        }
        TEXT_FINISHED => with_editor(|editor| editor.close_text(wparam != 0)),
        text::UPDATED => with_editor(|editor| {
            if let (Some(input), Some(crop)) = (&mut editor.text, editor.selection) {
                if input.hwnd == wparam as HWND {
                    input.refresh(hwnd, editor.frame.dc, &editor.font, crop);
                    editor.repaint();
                }
            }
        }),
        text::IME_POSITION if lparam != 0 => {
            let mut handled = false;
            with_editor(|editor| {
                if let (Some(input), Some(crop)) = (&editor.text, editor.selection) {
                    if input.hwnd == wparam as HWND {
                        input.ime_position(
                            crop,
                            editor.desktop,
                            &mut *(lparam as *mut IMECHARPOSITION),
                        );
                        handled = true;
                    }
                }
            });
            return LRESULT::from(handled);
        }
        WM_DISPLAYCHANGE | WM_CLOSE => with_editor(|editor| editor.finish(false)),
        WM_ACTIVATE if wparam as u16 == WA_INACTIVE as u16 => {
            with_editor(|editor| editor.finish(false))
        }
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

impl Editor {
    fn pointer(&self, position: LPARAM) -> Point {
        Point {
            x: (position as i16 as i32).clamp(0, self.desktop.width() - 1),
            y: ((position >> 16) as i16 as i32).clamp(0, self.desktop.height() - 1),
        }
    }

    fn mouse_down(&mut self, point: Point) {
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
        if std::mem::take(&mut self.palette_open) {
            self.repaint();
        }
        if !crop.contains(point) {
            self.close_text(true);
            return;
        }
        let local = crop.local(point);
        if let Some(input) = &self.text {
            if input.draft.bounds().contains(local) {
                input.set_caret(local);
                return;
            }
            self.close_text(true);
        }

        match self.tool {
            Tool::Select => {
                let handle = self.handle_at(local, crop.local_bounds());
                if handle.is_none() {
                    self.selected_mark = self.mark_at(local);
                }
                if let Some(index) = self.selected_mark {
                    self.begin_drag(point, DragKind::Mark { index, handle });
                }
                self.repaint();
            }
            Tool::Eraser => {
                if let Some(index) = self.mark_at(local) {
                    self.defer_click(ClickAction::Erase(index));
                }
            }
            Tool::Text => self.open_text(crop, local),
            tool => self.begin_drag(
                point,
                DragKind::Draw {
                    tool,
                    points: if tool == Tool::Pen {
                        vec![local]
                    } else {
                        Vec::new()
                    },
                },
            ),
        }
    }

    fn begin_drag(&mut self, point: Point, kind: DragKind) {
        self.drag = Some(Drag {
            start: point,
            end: point,
            kind,
        });
        unsafe { SetCapture(self.hwnd) };
        self.set_cursor(Some(point));
    }

    fn mouse_move(&mut self, point: Point) {
        let old_window = self.pointer.and_then(|old| self.window_at(old));
        if self.pointer.replace(point).is_none() {
            let mut tracking = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe { TrackMouseEvent(&mut tracking) };
        }
        if let Some(drag) = self.drag.as_mut() {
            drag.update(point, self.selection);
            if let DragKind::Toolbar { origin } = drag.kind {
                let position = origin + drag.delta();
                self.toolbar = Some(self.clamp_toolbar(position, point));
            }
            self.repaint();
        } else {
            let hovered = self.toolbar_layout().and_then(|layout| layout.hit(point));
            if self.hovered != hovered || old_window != self.window_at(point) {
                self.hovered = hovered;
                self.repaint();
            }
        }
        self.set_cursor(Some(point));
    }

    fn mark_at(&self, point: Point) -> Option<usize> {
        self.marks
            .iter()
            .rposition(|mark| mark.hit(point))
            .or_else(|| {
                self.selected_mark
                    .filter(|&index| self.marks[index].bounds().contains(point))
            })
            .or_else(|| {
                self.marks
                    .iter()
                    .rposition(|mark| mark.contains_rectangle(point))
            })
    }

    fn handle_at(&self, point: Point, crop: Area) -> Option<Handle> {
        self.selected_mark
            .and_then(|index| self.marks.get(index))
            .and_then(|mark| mark.handle_at(point, crop))
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
                DragKind::Toolbar { .. } | DragKind::Mark { handle: None, .. } => IDC_SIZEALL,
                DragKind::Mark {
                    handle: Some(handle),
                    ..
                } => handle.cursor(),
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
                    if let Some(handle) = self.handle_at(local, crop.local_bounds()) {
                        return handle.cursor();
                    }
                    return if self.mark_at(local).is_some() {
                        IDC_SIZEALL
                    } else {
                        IDC_ARROW
                    };
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

    fn transformed_mark(&self, crop: Area, drag: &Drag) -> Option<(usize, Mark)> {
        let DragKind::Mark { index, handle } = drag.kind else {
            return None;
        };
        let mark = self
            .marks
            .get(index)?
            .transform(handle, drag.delta(), crop.local_bounds())?;
        Some((index, mark))
    }

    fn mouse_up(&mut self, point: Point) {
        let Some(mut drag) = self.drag.take() else {
            return;
        };
        drag.update(point, self.selection);
        unsafe { ReleaseCapture() };
        match drag.kind {
            DragKind::Region { .. } => {
                self.selection = self.selection_for_drag(&drag);
                self.toolbar = self.selection.map(|crop| self.toolbar_origin(crop, point));
            }
            DragKind::Toolbar { origin } => {
                self.toolbar = Some(self.clamp_toolbar(origin + drag.delta(), point));
            }
            _ => {
                if let Some(crop) = self.selection {
                    if let Some(mark) = drag.drawn_mark(crop, self.color) {
                        self.add_mark(mark);
                    }
                    if let Some((index, mark)) = self.transformed_mark(crop, &drag) {
                        self.change(Change::Replace(index, mark));
                    }
                }
            }
        }
        self.mouse_move(point);
        self.repaint();
    }

    fn add_mark(&mut self, mark: Mark) {
        let index = self.marks.len();
        self.change(Change::Insert(index, mark));
        self.tool = Tool::Select;
        self.selected_mark = Some(index);
    }

    fn change(&mut self, change: Change) {
        self.undo.push(change.apply(&mut self.marks));
        self.redo.clear();
    }

    fn discard_drag(&mut self) {
        if self.drag.take().is_some() {
            unsafe { ReleaseCapture() };
        }
    }

    fn history(&mut self, redo: bool) {
        self.discard_drag();
        let (from, to) = if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        if let Some(change) = from.pop() {
            to.push(change.apply(&mut self.marks));
            self.selected_mark = None;
        }
    }

    fn erase(&mut self, index: usize) {
        self.discard_drag();
        if index < self.marks.len() {
            self.change(Change::Remove(index));
            self.selected_mark = None;
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
                if let Some(index) = self.selected_mark {
                    self.erase(index);
                }
            }
            0x5a if control => self.history(unsafe { GetKeyState(i32::from(VK_SHIFT)) < 0 }),
            0x59 if control => self.history(true),
            _ => return,
        }
        self.set_cursor(self.pointer);
        self.repaint();
    }

    fn toolbar_click(&mut self, button: Button, point: Point) {
        if !matches!(button, Button::Palette | Button::Swatch(_)) {
            self.close_text(true);
            self.palette_open = false;
        }
        if button == Button::Grip {
            if let Some(origin) = self.toolbar {
                self.begin_drag(point, DragKind::Toolbar { origin });
            }
        } else if matches!(button, Button::Cancel | Button::Undo | Button::Redo) {
            self.defer_click(ClickAction::Button(button));
        } else {
            self.perform_click(ClickAction::Button(button));
        }
    }

    fn perform_click(&mut self, action: ClickAction) {
        match action {
            ClickAction::Erase(index) => self.erase(index),
            ClickAction::Button(button) => match button {
                Button::Tool(tool) => {
                    self.tool = tool;
                    if tool != Tool::Select {
                        self.selected_mark = None;
                    }
                }
                Button::Palette => self.palette_open = !self.palette_open,
                Button::Swatch(color) => {
                    self.color = color;
                    self.palette_open = false;
                    if let Some(index) = self.selected_mark {
                        let mut mark = self.marks[index].clone();
                        if color != mark.color {
                            mark.color = color;
                            self.change(Change::Replace(index, mark));
                        }
                    }
                }
                Button::Undo => self.history(false),
                Button::Redo => self.history(true),
                Button::Cancel => self.finish(false),
                Button::Save => self.finish(true),
                Button::Grip => {}
            },
        }
        self.repaint();
    }

    fn defer_click(&mut self, action: ClickAction) {
        // A double-click takes priority over a first click that could discard work.
        if unsafe { SetTimer(self.hwnd, CLICK_TIMER, GetDoubleClickTime(), None) } != 0 {
            self.pending_click = Some(action);
        } else {
            self.perform_click(action);
        }
    }

    fn take_pending_click(&mut self) -> Option<ClickAction> {
        let action = self.pending_click.take()?;
        unsafe { KillTimer(self.hwnd, CLICK_TIMER) };
        Some(action)
    }

    fn apply_pending_click(&mut self) {
        if let Some(action) = self.take_pending_click() {
            self.perform_click(action);
        }
    }

    fn finish(&mut self, save: bool) {
        if save {
            if self.selection.is_none() {
                return;
            }
            self.close_text(true);
        }
        self.take_pending_click();
        self.finished = Some(save);
        unsafe { PostMessageW(self.owner, WM_CAPTURE_FINISHED, 0, 0) };
    }

    fn open_text(&mut self, crop: Area, anchor: Point) {
        self.text = TextInput::new(self.hwnd, self.frame.dc, &self.font, crop, anchor);
        if let Some(input) = self.text.as_mut() {
            unsafe { SetFocus(input.hwnd) };
            input.refresh(self.hwnd, self.frame.dc, &self.font, crop);
            self.repaint();
        }
    }

    fn close_text(&mut self, keep: bool) {
        let Some(input) = self.text.take() else {
            return;
        };
        if let Some(crop) = self.selection.filter(|_| keep) {
            if let Some(mark) = input.finish(self.frame.dc, &self.font, crop) {
                self.add_mark(Mark::text(mark, crop.local_bounds(), self.color));
            }
        }
        unsafe {
            DestroyWindow(input.hwnd);
            SetFocus(self.hwnd);
        }
        self.set_cursor(self.pointer);
        self.repaint();
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

    fn repaint(&self) {
        unsafe {
            InvalidateRect(self.hwnd, null(), 0);
        }
    }

    fn paint(&self, destination: HDC) {
        self.compose_frame();
        unsafe {
            BitBlt(
                destination,
                0,
                0,
                self.desktop.width(),
                self.desktop.height(),
                self.frame.dc,
                0,
                0,
                SRCCOPY,
            );
        }
    }

    fn compose_frame(&self) {
        let dc = self.frame.dc;
        let width = self.desktop.width();
        let height = self.desktop.height();
        unsafe {
            BitBlt(dc, 0, 0, width, height, self.shaded.dc, 0, 0, SRCCOPY);
        }
        let selection = self.selection.or_else(|| match &self.drag {
            Some(drag) => self.selection_for_drag(drag),
            None => self.pointer.and_then(|point| self.window_at(point)),
        });
        let Some(selection) = selection else { return };
        unsafe {
            BitBlt(
                dc,
                selection.left,
                selection.top,
                selection.width(),
                selection.height(),
                self.original.dc,
                selection.left,
                selection.top,
                SRCCOPY,
            );
            let state = SaveDC(dc);
            if state != 0 {
                SetViewportOrgEx(dc, selection.left, selection.top, null_mut());
                IntersectClipRect(dc, 0, 0, selection.width(), selection.height());
                let changed = self
                    .drag
                    .as_ref()
                    .and_then(|drag| self.transformed_mark(selection, drag));
                let visible_mark = |index: usize| {
                    changed
                        .as_ref()
                        .filter(|(changed_index, _)| *changed_index == index)
                        .map(|(_, mark)| mark)
                        .unwrap_or(&self.marks[index])
                };
                for index in 0..self.marks.len() {
                    visible_mark(index).paint(dc, &self.font, selection, self.antialias.as_ref());
                }
                if let Some(mark) = self
                    .drag
                    .as_ref()
                    .and_then(|drag| drag.drawn_mark(selection, self.color))
                {
                    mark.paint(dc, &self.font, selection, self.antialias.as_ref());
                }
                if let Some(index) = self.selected_mark {
                    visible_mark(index).paint_controls(dc, selection.local_bounds());
                }
                if let Some(input) = &self.text {
                    input.paint(dc, &self.font, selection, self.color);
                }
                RestoreDC(dc, state);
            }
            let border = CreateSolidBrush(appearance::ACCENT);
            if !border.is_null() {
                FrameRect(dc, &selection.rect(), border);
                DeleteObject(border);
            }
            if let Some(layout) = self.toolbar_layout() {
                let color = self
                    .selected_mark
                    .map_or(self.color, |index| self.marks[index].color);
                layout.paint(
                    dc,
                    toolbar::State {
                        active: self.tool,
                        hovered: self.hovered,
                        color,
                        can_undo: !self.undo.is_empty(),
                        can_redo: !self.redo.is_empty(),
                    },
                    self.antialias.as_ref(),
                );
            }
        }
    }

    fn export(&self) -> io::Result<Image> {
        let selection = self
            .selection
            .ok_or_else(|| io::Error::other("未选择截图区域"))?;
        let width = selection.width();
        let height = selection.height();
        let image = Bitmap::new(width, height)?;
        unsafe {
            if BitBlt(
                image.dc,
                0,
                0,
                width,
                height,
                self.original.dc,
                selection.left,
                selection.top,
                SRCCOPY,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            for mark in &self.marks {
                mark.paint(
                    image.dc,
                    &self.font,
                    selection.local_bounds(),
                    self.antialias.as_ref(),
                );
            }
            GdiFlush();
        }
        let mut rgb = Vec::with_capacity(image.length / 4 * 3);
        for pixel in image.bytes().chunks_exact(4) {
            rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        Ok(Image {
            width: width as u32,
            height: height as u32,
            rgb,
        })
    }
}

struct Font {
    handle: HFONT,
    chinese_width: i32,
    line_height: i32,
}

impl Font {
    fn new(dc: HDC) -> io::Result<Self> {
        let font = unsafe {
            CreateFontW(
                -ANNOTATION_FONT_HEIGHT,
                0,
                0,
                0,
                FW_NORMAL as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                ANTIALIASED_QUALITY as u32,
                DEFAULT_PITCH as u32,
                wide("Microsoft YaHei UI").as_ptr(),
            )
        };
        if font.is_null() {
            return Err(io::Error::last_os_error());
        }
        unsafe {
            let previous = SelectObject(dc, font);
            let mut metrics = TEXTMETRICW::default();
            GetTextMetricsW(dc, &mut metrics);
            let chinese_width = text::measure(dc, &['中' as u16]).max(1);
            SelectObject(dc, previous);
            Ok(Self {
                handle: font,
                chinese_width,
                line_height: metrics.tmHeight.max(1),
            })
        }
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.handle);
        }
    }
}

struct Bitmap {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    length: usize,
}

impl Bitmap {
    fn new(width: i32, height: i32) -> io::Result<Self> {
        let length = (i64::from(width) * i64::from(height))
            .checked_mul(4)
            .and_then(|bytes| usize::try_from(bytes).ok())
            .filter(|&length: &usize| width > 0 && height > 0 && length <= isize::MAX as usize)
            .ok_or_else(|| io::Error::other("屏幕尺寸无效"))?;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            if dc.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut bits = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bitmap.is_null() || bits.is_null() {
                let error = io::Error::last_os_error();
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                DeleteDC(dc);
                return Err(error);
            }
            let previous = SelectObject(dc, bitmap);
            if previous.is_null() {
                let error = io::Error::last_os_error();
                DeleteObject(bitmap);
                DeleteDC(dc);
                return Err(error);
            }
            let bits = bits.cast::<u8>();
            bits.write_bytes(0, length);
            Ok(Self {
                dc,
                bitmap,
                previous,
                bits,
                length,
            })
        }
    }

    fn capture(desktop: Area, monitors: &[Area]) -> io::Result<Self> {
        let bitmap = Self::new(desktop.width(), desktop.height())?;
        unsafe {
            let dc = GetDC(null_mut());
            if dc.is_null() {
                return Err(io::Error::last_os_error());
            }
            let result = monitors.iter().try_for_each(|monitor| {
                if BitBlt(
                    bitmap.dc,
                    monitor.left - desktop.left,
                    monitor.top - desktop.top,
                    monitor.width(),
                    monitor.height(),
                    dc,
                    monitor.left,
                    monitor.top,
                    SRCCOPY | CAPTUREBLT,
                ) == 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
            GdiFlush();
            ReleaseDC(null_mut(), dc);
            result?;
        }
        Ok(bitmap)
    }

    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.bits, self.length) }
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.length) }
    }
}

impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

#[cfg(test)]
mod tests {
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
        let frame = Bitmap::new(desktop.width(), desktop.height()).unwrap();
        Editor {
            hwnd: null_mut(),
            owner: null_mut(),
            previous: null_mut(),
            desktop,
            monitors: vec![desktop],
            windows: Vec::new(),
            original: Bitmap::new(desktop.width(), desktop.height()).unwrap(),
            shaded: Bitmap::new(desktop.width(), desktop.height()).unwrap(),
            font: Font::new(frame.dc).unwrap(),
            antialias: strokes::Antialias::new(),
            frame,
            selection: Some(selection),
            toolbar: None,
            palette_open: false,
            hovered: None,
            pointer: None,
            pending_click: None,
            drag: None,
            tool: Tool::Rectangle,
            color: INK,
            marks: Vec::new(),
            selected_mark: None,
            undo: Vec::new(),
            redo: Vec::new(),
            text: None,
            finished: None,
        }
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
    fn drawn_shapes_auto_select_and_support_moving_and_resizing() {
        let mut editor = editor_with_selection();
        let start = Point { x: 30, y: 42 };
        let end = Point { x: 72, y: 75 };
        assert_eq!(editor.tool, Tool::Rectangle);
        editor.mouse_down(start);
        editor.mouse_up(end);
        let original = editor.marks[0].bounds();
        assert_eq!(editor.tool, Tool::Select);
        assert_eq!(editor.selected_mark, Some(0));

        let east = Point { x: 72, y: 58 };
        assert_eq!(editor.cursor_at(Some(east)), IDC_SIZEWE);
        editor.mouse_down(east);
        editor.mouse_up(east + Point { x: 12, y: 0 });
        let resized = editor.marks[0].bounds();
        assert_eq!(resized.width(), original.width() + 12);
        assert_eq!(resized.height(), original.height());
        let center = Point { x: 55, y: 58 };
        let delta = Point { x: 7, y: 9 };
        editor.mouse_down(center);
        editor.mouse_up(center + delta);
        assert_eq!(editor.marks[0].bounds(), resized.offset(delta));
        editor.history(false);
        assert_eq!(editor.marks[0].bounds(), resized);
        editor.history(true);
        assert_eq!(editor.marks[0].bounds(), resized.offset(delta));

        let mut editor = editor_with_selection();
        editor.tool = Tool::Arrow;
        editor.mouse_down(start);
        editor.mouse_up(end);
        assert_eq!(editor.tool, Tool::Select);
        editor.mouse_down(end);
        editor.mouse_up(end + Point { x: 10, y: -12 });
        let changed = editor.marks[0].bounds();
        assert_eq!(changed.right, 73);
        assert_eq!(changed.bottom, 44);
        let middle = Point { x: 56, y: 53 };
        editor.mouse_down(middle);
        editor.mouse_up(middle + delta);
        assert_eq!(editor.marks[0].bounds(), changed.offset(delta));

        let mut editor = editor_with_selection();
        editor.tool = Tool::Pen;
        editor.mouse_down(start);
        editor.mouse_move(Point { x: 50, y: 53 });
        editor.mouse_up(end);
        assert_eq!(editor.tool, Tool::Select);
        let original = editor.marks[0].bounds();
        let south = Point { x: 51, y: 75 };
        assert_eq!(editor.cursor_at(Some(south)), IDC_SIZENS);
        editor.mouse_down(south);
        editor.mouse_up(south + Point { x: 0, y: 10 });
        assert_eq!(editor.marks[0].bounds().height(), original.height() + 10);
        let ink = rgb(17, 120, 250);
        editor.perform_click(ClickAction::Button(Button::Swatch(ink)));
        assert_eq!(editor.marks[0].color, ink);
        editor.erase(0);
        assert!(editor.marks.is_empty());
        editor.history(false);
        assert_eq!(editor.marks[0].color, ink);
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
        editor.marks = vec![
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
                    editor.frame.dc,
                    &editor.font,
                    selection,
                    Point { x: 8, y: 45 },
                    "中文".encode_utf16().collect(),
                ),
                selection.local_bounds(),
                INK,
            ),
        ];
        let image = editor.export().unwrap();
        assert_eq!((image.width, image.height), (120, 80));
        let pixel = |x: usize, y: usize| &image.rgb[(y * 120 + x) * 3..][..3];
        assert_eq!(pixel(22, 8), [231, 43, 52]);
        assert_eq!(pixel(22, 20), background);
        assert_eq!(pixel(70, 10), [231, 43, 52]);
        assert_eq!(pixel(9, 46), background);
        assert!((11..54).any(|x| (47..69).any(|y| {
            let p = pixel(x, y);
            p[0] > p[1] && p[0] > p[2]
        })));
        editor.compose_frame();
        unsafe {
            GdiFlush();
        }
        let preview_pixel = |x, y| {
            let index = ((selection.top + y) * desktop.width() + selection.left + x) as usize * 4;
            &editor.frame.bytes()[index..index + 3]
        };
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
        let moved = &editor.marks[2];
        assert_eq!(moved.bounds().right, selection.width());
        assert_eq!(moved.bounds().bottom, selection.height());
        assert_eq!(editor.cursor_at(Some(grab)), IDC_ARROW);
        let image = editor.export().unwrap();
        let old_pixel = (55 * image.width as usize + 11) * 3;
        assert_eq!(image.rgb[old_pixel..old_pixel + 3], background);

        use annotations::Edge::First;
        let scaled = editor.marks[2]
            .transform(
                Some(Handle::Frame(First, First)),
                Point { x: -16, y: -10 },
                selection.local_bounds(),
            )
            .unwrap();
        let bounds = scaled.bounds();
        editor.change(Change::Replace(2, scaled));
        let scaled_image = editor.export().unwrap();
        let red_pixels = |image: &Image| {
            (40..80)
                .flat_map(|y| (60..120).map(move |x| (x, y)))
                .filter(|&(x, y)| image.rgb[(y * 120 + x) * 3] > 180)
                .count()
        };
        assert!(red_pixels(&scaled_image) > red_pixels(&image));
        let control_x = bounds.left as usize;
        let control_y = bounds.top as usize;
        let exported = (control_y * 120 + control_x) * 3;
        assert_eq!(scaled_image.rgb[exported..exported + 3], background);
    }

    #[test]
    fn diagonal_strokes_blend_edges_in_preview_and_export() {
        let mut editor = editor_with_selection();
        assert!(editor.antialias.is_some());
        let crop = editor.selection.unwrap();
        let background = [20, 60, 160];
        for pixel in editor.original.bytes_mut().chunks_exact_mut(4) {
            pixel.copy_from_slice(&[background[2], background[1], background[0], 0]);
        }
        editor.marks = vec![
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
        ];
        let image = editor.export().unwrap();
        let blended = |from, to| {
            image.rgb.chunks_exact(3).enumerate().any(|(index, pixel)| {
                let x = index % image.width as usize;
                (from..to).contains(&x)
                    && (background[0] + 1..231).contains(&pixel[0])
                    && (53..background[2]).contains(&pixel[2])
            })
        };
        assert!(blended(0, 53));
        assert!(blended(60, 120));

        editor.compose_frame();
        unsafe { GdiFlush() };
        let preview = |x, y| {
            let index = (y * editor.desktop.width() + x) as usize * 4;
            &editor.frame.bytes()[index..index + 3]
        };
        for y in 1..crop.height() - 1 {
            for x in 1..crop.width() - 1 {
                let index = (y * crop.width() + x) as usize * 3;
                let [r, g, b] = image.rgb[index..index + 3] else {
                    unreachable!()
                };
                assert_eq!(preview(crop.left + x, crop.top + y), [b, g, r]);
            }
        }
        assert_eq!(preview(crop.left - 1, crop.top + 1), [0, 0, 0]);
    }

    #[test]
    fn double_click_finishes_on_canvas_toolbar_and_text_input() {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

        let mut editor = editor_with_selection();
        let instance = unsafe { GetModuleHandleW(null()) };
        register(instance).unwrap();
        let hwnd = unsafe {
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
        assert!(!hwnd.is_null());
        editor.hwnd = hwnd;
        editor.toolbar = Some(Point { x: 10, y: 107 });
        let selection = editor.selection.unwrap();
        let anchor = Point {
            x: selection.width() - 2 * editor.font.chinese_width - 5,
            y: 20,
        };
        editor.add_mark(Mark::text(
            TextMark::new(
                editor.frame.dc,
                &editor.font,
                selection,
                Point { x: 0, y: 0 },
                "test".encode_utf16().collect(),
            ),
            selection.local_bounds(),
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
        EDITOR.with(|slot| slot.replace(Some(editor)));

        let outside = Point { x: 0, y: 0 };
        with_editor(|editor| {
            let selection = editor.selection.take();
            editor.finish(true);
            assert_eq!(editor.finished, None);
            editor.selection = selection;
        });
        let check_saved = |mark_count| {
            EDITOR.with(|slot| {
                let mut slot = slot.borrow_mut();
                let editor = slot.as_mut().unwrap();
                assert_eq!(editor.finished.take(), Some(true));
                assert_eq!(editor.marks.len(), mark_count);
                assert_eq!(editor.tool, Tool::Select);
            });
        };
        for point in [outside, Point { x: 31, y: 35 }, cancel, undo] {
            send_mouse(WM_LBUTTONDOWN, point);
            send_mouse(WM_LBUTTONUP, point);
            send_mouse(WM_LBUTTONDBLCLK, point);
            check_saved(1);
        }

        let mut edit = null_mut();
        with_editor(|editor| {
            editor.tool = Tool::Text;
            editor.open_text(selection, anchor);
            let input = editor.text.as_ref().unwrap();
            edit = input.hwnd;
            assert_eq!(input.draft.bounds().width(), editor.font.chinese_width);
        });
        unsafe {
            for unit in "中文标注测试".encode_utf16() {
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
            let bounds = editor.text.as_ref().unwrap().draft.bounds();
            assert!(bounds.width() > editor.font.chinese_width);
            assert!(bounds.right <= selection.width());
            assert!(bounds.height() > editor.font.line_height);
            assert_eq!(
                crate::control_text(edit),
                "中文标注测试".encode_utf16().collect::<Vec<_>>()
            );
        });
        let draft_point = Point {
            x: selection.left + anchor.x + 1,
            y: selection.top + anchor.y + 1,
        };
        send_mouse(WM_LBUTTONDOWN, draft_point);
        send_mouse(WM_LBUTTONUP, draft_point);
        send_mouse(WM_LBUTTONDBLCLK, draft_point);
        check_saved(2);

        send_mouse(WM_LBUTTONDOWN, cancel);
        send_mouse(WM_LBUTTONUP, cancel);
        unsafe { SendMessageW(hwnd, WM_TIMER, CLICK_TIMER, 0) };
        assert!(complete().unwrap().is_none());
    }
}
