use super::{strokes, Area, Font, Point, TextMark};
use crate::appearance::{fill, rgb, WHITE};
use std::rc::Rc;
use windows_sys::Win32::Foundation::COLORREF;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const HANDLE_RADIUS: i32 = 4;
const HIT_RADIUS: i32 = 6;
const SELECTION: COLORREF = rgb(23, 168, 158);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tool {
    Select,
    Rectangle,
    Arrow,
    Pen,
    Text,
    Eraser,
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
pub(super) enum Handle {
    Frame(Edge, Edge),
    Tail,
    Tip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edge {
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

    pub fn text(text: TextMark, crop: Area, color: COLORREF) -> Self {
        let source = text.bounds().intersection(crop).unwrap();
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

    pub fn hit(&self, point: Point) -> bool {
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

    pub fn contains_rectangle(&self, point: Point) -> bool {
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

    pub fn handle_at(&self, point: Point, crop: Area) -> Option<Handle> {
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

    pub fn transform(&self, handle: Option<Handle>, delta: Point, crop: Area) -> Option<Self> {
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
        font: &Font,
        crop: Area,
        antialias: Option<&strokes::Antialias>,
    ) {
        if let Shape::Text {
            text,
            source,
            bounds,
        } = &self.shape
        {
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
            let pen = CreatePen(PS_DOT, 1, SELECTION);
            if !pen.is_null() {
                let previous = SelectObject(dc, pen);
                let brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
                let mode = SetBkMode(dc, TRANSPARENT as i32);
                Rectangle(dc, frame.left, frame.top, frame.right, frame.bottom);
                SetBkMode(dc, mode);
                SelectObject(dc, brush);
                SelectObject(dc, previous);
                DeleteObject(pen);
            }
        }
        for (_, center) in self.handles(crop) {
            let outer = point_bounds(
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
            fill(dc, &outer.rect(), SELECTION);
            let inner = Area {
                left: outer.left + 1,
                top: outer.top + 1,
                right: outer.right - 1,
                bottom: outer.bottom - 1,
            };
            fill(dc, &inner.rect(), WHITE);
        }
    }
}

impl Handle {
    pub fn cursor(self) -> *const u16 {
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

pub(super) enum Change {
    Insert(usize, Mark),
    Remove(usize),
    Replace(usize, Mark),
}

impl Change {
    pub fn apply(self, marks: &mut Vec<Mark>) -> Self {
        match self {
            Self::Insert(index, mark) => {
                marks.insert(index, mark);
                Self::Remove(index)
            }
            Self::Remove(index) => Self::Insert(index, marks.remove(index)),
            Self::Replace(index, mark) => {
                Self::Replace(index, std::mem::replace(&mut marks[index], mark))
            }
        }
    }
}
