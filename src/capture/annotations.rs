use super::{strokes, text::TextMark, Area, Font, Point};
use crate::appearance::{fill, rgb, WHITE};
use std::rc::Rc;
use windows_sys::Win32::Foundation::COLORREF;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const HANDLE_RADIUS: i32 = 4;
const HIT_RADIUS: i32 = 6;
const CONTROL_OUTLINE: COLORREF = rgb(24, 24, 28);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tool {
    Select,
    Rectangle,
    Arrow,
    Pen,
    Text,
}

#[derive(Clone)]
pub(super) struct Mark {
    pub color: COLORREF,
    shape: Shape,
}

#[derive(Clone)]
enum Shape {
    Rectangle(Area),
    Arrow(Point, Point),
    Pen {
        points: Rc<[Point]>,
        source: Area,
        bounds: Area,
    },
    Text {
        text: Rc<TextMark>,
        source: Area,
        bounds: Area,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handle {
    Frame(Edge, Edge),
    Tail,
    Tip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    First,
    Center,
    Last,
}

impl Mark {
    pub fn rectangle(bounds: Area, color: COLORREF) -> Self {
        Self {
            color,
            shape: Shape::Rectangle(bounds),
        }
    }

    pub fn arrow(start: Point, end: Point, color: COLORREF) -> Self {
        Self {
            color,
            shape: Shape::Arrow(start, end),
        }
    }

    pub fn pen(points: Vec<Point>, color: COLORREF) -> Self {
        let bounds = points
            .iter()
            .copied()
            .fold(point_bounds(points[0], points[0]), |area, p| Area {
                left: area.left.min(p.x),
                top: area.top.min(p.y),
                right: area.right.max(p.x + 1),
                bottom: area.bottom.max(p.y + 1),
            });
        Self {
            color,
            shape: Shape::Pen {
                points: points.into(),
                source: bounds,
                bounds,
            },
        }
    }

    pub fn text(text: TextMark, color: COLORREF) -> Self {
        let source = text.bounds();
        Self {
            color,
            shape: Shape::Text {
                text: Rc::new(text),
                source,
                bounds: source,
            },
        }
    }

    pub fn bounds(&self) -> Area {
        match self.shape {
            Shape::Rectangle(bounds) | Shape::Pen { bounds, .. } | Shape::Text { bounds, .. } => {
                bounds
            }
            Shape::Arrow(start, end) => point_bounds(start, end),
        }
    }

    pub fn paint_bounds(&self) -> Area {
        match self.shape {
            Shape::Text { bounds, .. } => bounds,
            Shape::Arrow(start, end) => {
                let [left, _, right] = arrow_head(start, end);
                self.bounds().union(point_bounds(left, right)).padded(3)
            }
            _ => self.bounds().padded(3),
        }
    }

    pub fn controls_bounds(&self, crop: Area) -> Area {
        if let Shape::Arrow(tail, tip) = self.shape {
            point_bounds(handle_center(tail, crop), handle_center(tip, crop)).padded(HANDLE_RADIUS)
        } else {
            control_frame(self.bounds(), crop).padded(HANDLE_RADIUS)
        }
    }

    fn hit(&self, point: Point) -> bool {
        match &self.shape {
            Shape::Rectangle(bounds) => {
                let corners = [bounds.left, bounds.right - 1].map(|x| {
                    [
                        Point { x, y: bounds.top },
                        Point {
                            x,
                            y: bounds.bottom - 1,
                        },
                    ]
                });
                let [a, b] = corners[0];
                let [c, d] = corners[1];
                [a, b, d, c, a]
                    .windows(2)
                    .any(|p| near_segment(point, p[0], p[1]))
            }
            Shape::Arrow(start, end) => {
                let [left, tip, right] = arrow_head(*start, *end);
                near_segment(point, *start, *end)
                    || near_segment(point, left, tip)
                    || near_segment(point, tip, right)
            }
            Shape::Pen {
                points,
                source,
                bounds,
            } => {
                if !bounds.padded(HIT_RADIUS).contains(point) {
                    return false;
                }
                let map = |p| scale_point(p, *source, *bounds);
                if points.len() == 1 {
                    let p = map(points[0]);
                    return near_segment(point, p, p);
                }
                points
                    .windows(2)
                    .any(|p| near_segment(point, map(p[0]), map(p[1])))
            }
            Shape::Text { bounds, .. } => bounds.contains(point),
        }
    }

    fn contains_rectangle(&self, point: Point) -> bool {
        matches!(self.shape, Shape::Rectangle(bounds) if bounds.contains(point))
    }

    fn handles(&self, crop: Area) -> Vec<(Handle, Point)> {
        if let Shape::Arrow(tail, tip) = self.shape {
            return vec![
                (Handle::Tail, handle_center(tail, crop)),
                (Handle::Tip, handle_center(tip, crop)),
            ];
        }
        use Edge::{Center, First, Last};
        let frame = control_frame(self.bounds(), crop);
        [
            (First, First),
            (Center, First),
            (Last, First),
            (First, Center),
            (Last, Center),
            (First, Last),
            (Center, Last),
            (Last, Last),
        ]
        .map(|(x, y)| {
            (
                Handle::Frame(x, y),
                Point {
                    x: x.position(frame.left, frame.right - 1),
                    y: y.position(frame.top, frame.bottom - 1),
                },
            )
        })
        .into()
    }

    fn handle_at(&self, point: Point, crop: Area) -> Option<Handle> {
        self.handles(crop)
            .into_iter()
            .filter_map(|(handle, center)| {
                let delta = point - center;
                (delta.x.abs() <= HIT_RADIUS && delta.y.abs() <= HIT_RADIUS)
                    .then_some((delta.x * delta.x + delta.y * delta.y, handle))
            })
            .min_by_key(|&(distance, _)| distance)
            .map(|(_, handle)| handle)
    }

    fn transform(&self, handle: Option<Handle>, delta: Point, crop: Area) -> Option<Self> {
        let bounds = self.bounds();
        let shape = match (&self.shape, handle) {
            (Shape::Arrow(start, end), Some(Handle::Tail | Handle::Tip)) => {
                let tail = handle == Some(Handle::Tail);
                let active = if tail { *start } else { *end };
                let fixed = if tail { *end } else { *start };
                let moved = crop.local(active + delta);
                if moved == fixed || moved == active {
                    return None;
                }
                if tail {
                    Shape::Arrow(moved, fixed)
                } else {
                    Shape::Arrow(fixed, moved)
                }
            }
            (_, Some(Handle::Frame(x, y))) => {
                let (left, right) = x.resize(bounds.left, bounds.right, delta.x, crop.width());
                let (top, bottom) = y.resize(bounds.top, bounds.bottom, delta.y, crop.height());
                self.reframe(Area {
                    left,
                    top,
                    right,
                    bottom,
                })?
            }
            (_, None) => {
                let shift = Point {
                    x: delta.x.clamp(-bounds.left, crop.right - bounds.right),
                    y: delta.y.clamp(-bounds.top, crop.bottom - bounds.bottom),
                };
                self.reframe(bounds.offset(shift))?
            }
            _ => return None,
        };
        Some(Self {
            color: self.color,
            shape,
        })
    }

    fn reframe(&self, bounds: Area) -> Option<Shape> {
        let old = self.bounds();
        if bounds == old {
            return None;
        }
        Some(match &self.shape {
            Shape::Rectangle(_) => Shape::Rectangle(bounds),
            Shape::Arrow(start, end) => {
                let shift = Point {
                    x: bounds.left - old.left,
                    y: bounds.top - old.top,
                };
                Shape::Arrow(*start + shift, *end + shift)
            }
            Shape::Pen { points, source, .. } => Shape::Pen {
                points: Rc::clone(points),
                source: *source,
                bounds,
            },
            Shape::Text { text, source, .. } => Shape::Text {
                text: Rc::clone(text),
                source: *source,
                bounds,
            },
        })
    }

    pub unsafe fn paint(
        &self,
        dc: HDC,
        font: Option<&Font>,
        crop: Area,
        antialias: Option<&strokes::Antialias>,
    ) {
        if let Shape::Text {
            text,
            source,
            bounds,
        } = &self.shape
        {
            let Some(font) = font else { return };
            let state = SaveDC(dc);
            if state == 0 {
                return;
            }
            IntersectClipRect(dc, bounds.left, bounds.top, bounds.right, bounds.bottom);
            if (source.width(), source.height()) == (bounds.width(), bounds.height()) {
                text.paint(
                    dc,
                    font,
                    Point {
                        x: bounds.left,
                        y: bounds.top,
                    },
                    self.color,
                );
            } else {
                let transform = XFORM {
                    eM11: bounds.width() as f32 / source.width() as f32,
                    eM22: bounds.height() as f32 / source.height() as f32,
                    eDx: bounds.left as f32,
                    eDy: bounds.top as f32,
                    ..Default::default()
                };
                if SetGraphicsMode(dc, GM_ADVANCED) != 0 && SetWorldTransform(dc, &transform) != 0 {
                    text.paint(dc, font, Point { x: 0, y: 0 }, self.color);
                }
            }
            RestoreDC(dc, state);
            return;
        }
        match &self.shape {
            Shape::Rectangle(bounds) => {
                let pen = CreatePen(PS_SOLID, 3, self.color);
                if pen.is_null() {
                    return;
                }
                let previous = SelectObject(dc, pen);
                let brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
                Rectangle(dc, bounds.left, bounds.top, bounds.right, bounds.bottom);
                SelectObject(dc, brush);
                SelectObject(dc, previous);
                DeleteObject(pen);
            }
            Shape::Arrow(start, end) => {
                let line = [*start, *end];
                let wings = arrow_head(*start, *end);
                strokes::paint(dc, crop, self.color, antialias, &[&line, &wings]);
            }
            Shape::Pen {
                points,
                source,
                bounds,
            } => {
                if points.len() > 1 && source == bounds {
                    strokes::paint(dc, crop, self.color, antialias, &[points]);
                    return;
                }
                let mut line: Vec<_> = points
                    .iter()
                    .map(|&p| scale_point(p, *source, *bounds))
                    .collect();
                if line.len() == 1 {
                    line.push(line[0] + Point { x: 1, y: 0 });
                }
                strokes::paint(dc, crop, self.color, antialias, &[&line]);
            }
            Shape::Text { .. } => unreachable!(),
        }
    }

    pub unsafe fn paint_controls(&self, dc: HDC, crop: Area) {
        if !matches!(self.shape, Shape::Arrow(..)) {
            let frame = control_frame(self.bounds(), crop);
            let brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
            let mode = SetBkMode(dc, TRANSPARENT as i32);
            for (style, color) in [(PS_SOLID, CONTROL_OUTLINE), (PS_DOT, WHITE)] {
                let pen = CreatePen(style, 1, color);
                if !pen.is_null() {
                    let previous = SelectObject(dc, pen);
                    Rectangle(dc, frame.left, frame.top, frame.right, frame.bottom);
                    SelectObject(dc, previous);
                    DeleteObject(pen);
                }
            }
            SetBkMode(dc, mode);
            SelectObject(dc, brush);
        }
        for (_, center) in self.handles(crop) {
            let bounds = point_bounds(
                center
                    - Point {
                        x: HANDLE_RADIUS,
                        y: HANDLE_RADIUS,
                    },
                center
                    + Point {
                        x: HANDLE_RADIUS,
                        y: HANDLE_RADIUS,
                    },
            );
            fill(dc, &bounds.rect(), CONTROL_OUTLINE);
            fill(dc, &bounds.padded(-1).rect(), WHITE);
        }
    }
}

impl Handle {
    fn cursor(self) -> *const u16 {
        use Edge::Center;
        match self {
            Self::Tail | Self::Tip => IDC_SIZEALL,
            Self::Frame(Center, _) => IDC_SIZENS,
            Self::Frame(_, Center) => IDC_SIZEWE,
            Self::Frame(x, y) if x == y => IDC_SIZENWSE,
            Self::Frame(_, _) => IDC_SIZENESW,
        }
    }
}

impl Edge {
    fn position(self, first: i32, last: i32) -> i32 {
        match self {
            Self::First => first,
            Self::Center => first + (last - first) / 2,
            Self::Last => last,
        }
    }

    fn resize(self, first: i32, last: i32, delta: i32, limit: i32) -> (i32, i32) {
        // An existing tiny mark can grow on either side without jumping on mouse-down.
        let minimum = (last - first).min(6);
        match self {
            Self::First => ((first + delta).clamp(0, last - minimum), last),
            Self::Center => (first, last),
            Self::Last => (first, (last + delta).clamp(first + minimum, limit)),
        }
    }
}

fn control_frame(bounds: Area, crop: Area) -> Area {
    let axis = |first: i32, end: i32, limit: i32| {
        let inset = HANDLE_RADIUS.min((limit - 1) / 2);
        let available = limit - 1 - 2 * inset;
        let span = (end - first - 1).max(24).min(available);
        let start = ((first + end - 1 - span) / 2).clamp(inset, limit - 1 - inset - span);
        (start, start + span)
    };
    let (left, right) = axis(bounds.left, bounds.right, crop.width());
    let (top, bottom) = axis(bounds.top, bounds.bottom, crop.height());
    Area {
        left,
        top,
        right: right + 1,
        bottom: bottom + 1,
    }
}

fn handle_center(point: Point, crop: Area) -> Point {
    let inset = |coord: i32, limit: i32| {
        let margin = HANDLE_RADIUS.min((limit - 1) / 2);
        coord.clamp(margin, limit - 1 - margin)
    };
    Point {
        x: inset(point.x, crop.width()),
        y: inset(point.y, crop.height()),
    }
}

fn point_bounds(a: Point, b: Point) -> Area {
    Area {
        left: a.x.min(b.x),
        top: a.y.min(b.y),
        right: a.x.max(b.x) + 1,
        bottom: a.y.max(b.y) + 1,
    }
}

fn scale_point(point: Point, source: Area, target: Area) -> Point {
    let scale = |offset: i32, from: i32, to: i32| {
        if from == 1 {
            (to - 1) / 2
        } else {
            (f64::from(offset) * f64::from(to - 1) / f64::from(from - 1)).round() as i32
        }
    };
    Point {
        x: target.left + scale(point.x - source.left, source.width(), target.width()),
        y: target.top + scale(point.y - source.top, source.height(), target.height()),
    }
}

fn near_segment(point: Point, start: Point, end: Point) -> bool {
    let segment = end - start;
    let relative = point - start;
    let (dx, dy) = (f64::from(segment.x), f64::from(segment.y));
    let (x, y) = (f64::from(relative.x), f64::from(relative.y));
    let length_squared = dx * dx + dy * dy;
    let t = if length_squared == 0.0 {
        0.0
    } else {
        ((x * dx + y * dy) / length_squared).clamp(0.0, 1.0)
    };
    (x - t * dx).hypot(y - t * dy) <= f64::from(HIT_RADIUS)
}

fn arrow_head(start: Point, end: Point) -> [Point; 3] {
    let delta = end - start;
    let (dx, dy) = (f64::from(delta.x), f64::from(delta.y));
    let length = dx.hypot(dy);
    let scale = if length == 0.0 {
        0.0
    } else {
        13.0_f64.min(length * 0.55) / length
    };
    let x = dx * scale;
    let y = dy * scale;
    [
        Point {
            x: end.x - (x - y * 0.55).round() as i32,
            y: end.y - (y + x * 0.55).round() as i32,
        },
        end,
        Point {
            x: end.x - (x + y * 0.55).round() as i32,
            y: end.y - (y - x * 0.55).round() as i32,
        },
    ]
}

#[derive(Default)]
pub(super) struct Annotations {
    marks: Vec<Mark>,
    selected: Option<usize>,
    gesture: Option<Gesture>,
    pen_id: u64,
    undo: Vec<Change>,
    redo: Vec<Change>,
    changed_bounds: Vec<Area>,
}

enum GestureKind {
    Rectangle,
    Arrow,
    Pen(strokes::LivePen),
    Transform {
        index: usize,
        handle: Option<Handle>,
    },
}

struct Gesture {
    start: Point,
    end: Point,
    crop: Area,
    kind: GestureKind,
}

impl Gesture {
    fn shape(&self, color: COLORREF) -> Option<Mark> {
        match self.kind {
            GestureKind::Rectangle => {
                Area::from_drag(self.start, self.end).map(|bounds| Mark::rectangle(bounds, color))
            }
            GestureKind::Arrow if self.start != self.end => {
                Some(Mark::arrow(self.start, self.end, color))
            }
            _ => None,
        }
    }
}

pub(super) enum LiveDrawing<'a> {
    Pen {
        id: u64,
        stroke: &'a strokes::LivePen,
    },
    Shape(Mark),
}

pub(super) struct AnnotationView<'a> {
    marks: &'a [Mark],
    selected: Option<usize>,
    replacement: Option<(usize, Mark)>,
    pub drawing: Option<LiveDrawing<'a>>,
}

impl AnnotationView<'_> {
    fn visible(&self, index: usize) -> &Mark {
        self.replacement
            .as_ref()
            .filter(|(changed, _)| *changed == index)
            .map_or(&self.marks[index], |(_, mark)| mark)
    }

    pub fn marks(&self) -> impl Iterator<Item = &Mark> {
        (0..self.marks.len()).map(|index| self.visible(index))
    }

    pub fn selected(&self) -> Option<(usize, &Mark)> {
        let index = self.selected?;
        Some((index, self.visible(index)))
    }

    pub fn replacement(&self) -> Option<(&Mark, &Mark)> {
        let (index, mark) = self.replacement.as_ref()?;
        Some((&self.marks[*index], mark))
    }
}

impl Annotations {
    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }

    pub fn selected(&self) -> Option<&Mark> {
        self.selected.map(|index| &self.marks[index])
    }

    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    pub fn view(&self, color: COLORREF) -> AnnotationView<'_> {
        let mut view = AnnotationView {
            marks: &self.marks,
            selected: self.selected,
            replacement: None,
            drawing: None,
        };
        if let Some(gesture) = &self.gesture {
            match &gesture.kind {
                GestureKind::Transform { index, handle } => {
                    view.replacement = self
                        .transformed(gesture, *index, *handle)
                        .map(|mark| (*index, mark));
                }
                GestureKind::Pen(stroke) => {
                    view.drawing = Some(LiveDrawing::Pen {
                        id: self.pen_id,
                        stroke,
                    });
                }
                _ => view.drawing = gesture.shape(color).map(LiveDrawing::Shape),
            }
        }
        view
    }

    fn hit(&self, point: Point) -> Option<usize> {
        self.marks
            .iter()
            .rposition(|mark| mark.hit(point))
            .or_else(|| {
                self.selected
                    .filter(|&index| self.marks[index].bounds().contains(point))
            })
            .or_else(|| {
                self.marks
                    .iter()
                    .rposition(|mark| mark.contains_rectangle(point))
            })
    }

    pub fn cursor_at(&self, point: Point, crop: Area) -> *const u16 {
        if let Some(handle) = self.selected().and_then(|mark| mark.handle_at(point, crop)) {
            handle.cursor()
        } else if self.hit(point).is_some() {
            IDC_SIZEALL
        } else {
            IDC_ARROW
        }
    }

    pub fn begin_gesture(&mut self, tool: Tool, point: Point, crop: Area) -> bool {
        if self.gesture.is_some() {
            return false;
        }
        let kind = match tool {
            Tool::Select => {
                let handle = self.selected().and_then(|mark| mark.handle_at(point, crop));
                if handle.is_none() {
                    self.selected = self.hit(point);
                }
                let Some(index) = self.selected else {
                    return false;
                };
                GestureKind::Transform { index, handle }
            }
            Tool::Rectangle => GestureKind::Rectangle,
            Tool::Arrow => GestureKind::Arrow,
            Tool::Pen => {
                self.pen_id = self.pen_id.wrapping_add(1);
                GestureKind::Pen(strokes::LivePen::new(point))
            }
            Tool::Text => return false,
        };
        self.gesture = Some(Gesture {
            start: point,
            end: point,
            crop,
            kind,
        });
        true
    }

    pub fn has_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    pub fn gesture_cursor(&self) -> Option<*const u16> {
        Some(match self.gesture.as_ref()?.kind {
            GestureKind::Transform { handle, .. } => handle.map_or(IDC_SIZEALL, Handle::cursor),
            _ => IDC_CROSS,
        })
    }

    pub fn update_gesture(&mut self, point: Point) {
        if let Some(gesture) = &mut self.gesture {
            gesture.end = point;
            if let GestureKind::Pen(stroke) = &mut gesture.kind {
                stroke.push(point);
            }
        }
    }

    fn transformed(&self, gesture: &Gesture, index: usize, handle: Option<Handle>) -> Option<Mark> {
        self.marks
            .get(index)?
            .transform(handle, gesture.end - gesture.start, gesture.crop)
    }

    pub fn commit_gesture(&mut self, color: COLORREF) -> bool {
        let Some(gesture) = self.gesture.take() else {
            return false;
        };
        match gesture.kind {
            GestureKind::Transform { index, handle } => {
                if let Some(mark) = self.transformed(&gesture, index, handle) {
                    self.change(Change::Replace(index, mark));
                }
            }
            GestureKind::Pen(stroke) => {
                if stroke.points.len() > 1 {
                    self.add(Mark::pen(stroke.points, color));
                }
            }
            _ => {
                if let Some(mark) = gesture.shape(color) {
                    self.add(mark);
                }
            }
        }
        true
    }

    pub fn cancel_gesture(&mut self) -> bool {
        self.gesture.take().is_some()
    }

    pub fn add(&mut self, mark: Mark) {
        self.change(Change::Insert(self.marks.len(), mark));
    }

    pub fn delete_selected(&mut self) {
        if let Some(index) = self.selected.take() {
            self.change(Change::Remove(index));
        }
    }

    pub fn recolor_selected(&mut self, color: COLORREF) {
        if let Some(index) = self
            .selected
            .filter(|&index| self.marks[index].color != color)
        {
            let mut mark = self.marks[index].clone();
            mark.color = color;
            self.change(Change::Replace(index, mark));
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) {
        if let Some(change) = self.undo.pop() {
            let inverse = self.apply(change);
            self.redo.push(inverse);
            self.clear_selection();
        }
    }

    pub fn redo(&mut self) {
        if let Some(change) = self.redo.pop() {
            let inverse = self.apply(change);
            self.undo.push(inverse);
            self.clear_selection();
        }
    }

    pub fn drain_changed_bounds(&mut self) -> impl Iterator<Item = Area> + '_ {
        self.changed_bounds.drain(..)
    }

    fn change(&mut self, change: Change) {
        let inverse = self.apply(change);
        self.undo.push(inverse);
        self.redo.clear();
    }

    fn apply(&mut self, change: Change) -> Change {
        let (inverse, before, after) = change.apply(&mut self.marks);
        self.changed_bounds.extend(before);
        self.changed_bounds
            .extend(after.filter(|area| Some(*area) != before));
        inverse
    }
}

enum Change {
    Insert(usize, Mark),
    Remove(usize),
    Replace(usize, Mark),
}

impl Change {
    fn apply(self, marks: &mut Vec<Mark>) -> (Self, Option<Area>, Option<Area>) {
        match self {
            Self::Insert(index, mark) => {
                let after = mark.paint_bounds();
                marks.insert(index, mark);
                (Self::Remove(index), None, Some(after))
            }
            Self::Remove(index) => {
                let removed = marks.remove(index);
                let before = removed.paint_bounds();
                (Self::Insert(index, removed), Some(before), None)
            }
            Self::Replace(index, mark) => {
                let after = mark.paint_bounds();
                let replaced = std::mem::replace(&mut marks[index], mark);
                let before = replaced.paint_bounds();
                (Self::Replace(index, replaced), Some(before), Some(after))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topmost_mark_wins_and_a_new_edit_clears_redo() {
        let mut annotations = Annotations::default();
        let crop = Area {
            left: 0,
            top: 0,
            right: 120,
            bottom: 100,
        };
        let bounds = Area {
            left: 20,
            top: 20,
            right: 80,
            bottom: 60,
        };
        let point = Point { x: 45, y: 20 };
        let delta = Point { x: 9, y: 12 };
        let back = rgb(20, 40, 60);
        let front = rgb(100, 120, 140);
        let replacement = rgb(150, 170, 190);
        annotations.add(Mark::rectangle(bounds, back));
        annotations.add(Mark::rectangle(bounds, front));

        assert!(annotations.begin_gesture(Tool::Select, point, crop));
        annotations.update_gesture(point + delta);
        let view = annotations.view(front);
        assert_eq!(
            view.marks().map(Mark::bounds).collect::<Vec<_>>(),
            [bounds, bounds.offset(delta)]
        );
        assert_eq!(annotations.selected().unwrap().color, front);
        assert!(annotations.commit_gesture(front));
        assert_eq!(annotations.marks()[1].bounds(), bounds.offset(delta));

        annotations.undo();
        assert!(annotations.selected().is_none());
        assert_eq!(annotations.marks()[1].bounds(), bounds);
        assert!(annotations.can_redo());
        assert!(annotations.begin_gesture(Tool::Select, point, crop));
        assert!(annotations.cancel_gesture());
        annotations.recolor_selected(front);
        assert!(annotations.can_redo());
        annotations.recolor_selected(replacement);
        assert!(!annotations.can_redo());

        annotations.delete_selected();
        assert_eq!(annotations.marks().len(), 1);
        annotations.undo();
        assert_eq!(
            annotations
                .marks()
                .iter()
                .map(|mark| mark.color)
                .collect::<Vec<_>>(),
            [back, replacement],
        );
    }
}
