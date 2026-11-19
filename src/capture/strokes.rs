use super::{Area, Point};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{COLORREF, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Graphics::GdiPlus::{self as gdip, *};

pub(super) use gdip::PointF;

const STROKE_WIDTH: i32 = 3;
pub(super) const PEN_MARGIN: i32 = 3;
const SEGMENTS_PER_BLOCK: usize = 32;

pub(super) struct LivePen {
    pub points: Vec<Point>,
    bounds: Area,
    blocks: Vec<Area>,
}

impl LivePen {
    pub fn new(point: Point) -> Self {
        Self {
            points: vec![point],
            bounds: Area::pixel(point),
            blocks: Vec::new(),
        }
    }

    pub fn push(&mut self, point: Point) {
        let previous = *self.points.last().unwrap();
        if previous == point {
            return;
        }
        let pixel = Area::pixel(point);
        let segment = Area::pixel(previous).union(pixel);
        if (self.points.len() - 1) % SEGMENTS_PER_BLOCK == 0 {
            self.blocks.push(segment);
        } else if let Some(block) = self.blocks.last_mut() {
            *block = block.union(segment);
        }
        self.bounds = self.bounds.union(pixel);
        self.points.push(point);
    }

    pub fn paint_bounds(&self) -> Option<Area> {
        (self.points.len() > 1).then(|| self.bounds.padded(PEN_MARGIN))
    }

    pub fn visible_points(&self, clip: Area) -> Option<&[Point]> {
        let hits = |bounds: &Area| bounds.padded(PEN_MARGIN).intersection(clip).is_some();
        let first = self.blocks.iter().position(hits)?;
        let last = self.blocks.iter().rposition(hits)?;
        // Keep intervening points for self-crossings and a segment at each end for joins.
        let start = (first * SEGMENTS_PER_BLOCK).saturating_sub(1);
        let end = ((last + 1) * SEGMENTS_PER_BLOCK + 2).min(self.points.len());
        Some(&self.points[start..end])
    }
}

pub(super) struct Antialias(usize);

pub(super) struct Canvas(*mut GpGraphics);

impl Antialias {
    pub fn new() -> Option<Self> {
        let mut token = 0;
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            ..Default::default()
        };
        let result = unsafe { GdiplusStartup(&mut token, &input, null_mut()) };
        if result == gdip::Ok {
            Some(Self(token))
        } else {
            None
        }
    }

    pub unsafe fn draw(&self, dc: HDC, clip: Area, paint: impl FnOnce(&Canvas) -> bool) -> bool {
        // GDI+ draws in device coordinates. Preserve the logical origin of the tile or crop.
        let state = SaveDC(dc);
        if state == 0 {
            return false;
        }
        let mut viewport = POINT::default();
        SetViewportOrgEx(dc, 0, 0, &mut viewport);
        let origin = Point {
            x: viewport.x,
            y: viewport.y,
        };
        let mut rect = RECT::default();
        let region = GetClipBox(dc, &mut rect);
        if region == 0 {
            RestoreDC(dc, state);
            return false;
        }
        let Some(clip) = clip.offset(origin).intersection(Area::from_rect(rect)) else {
            RestoreDC(dc, state);
            return true;
        };
        let clip = clip.offset(Point {
            x: -origin.x,
            y: -origin.y,
        });
        let mut graphics = null_mut();
        let drawn = if GdipCreateFromHDC(dc, &mut graphics) == gdip::Ok {
            let configured = GdipSetPageUnit(graphics, UnitPixel) == gdip::Ok
                && GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias8x8) == gdip::Ok
                && GdipTranslateWorldTransform(
                    graphics,
                    origin.x as f32,
                    origin.y as f32,
                    MatrixOrderPrepend,
                ) == gdip::Ok
                && GdipSetClipRectI(
                    graphics,
                    clip.left,
                    clip.top,
                    clip.width(),
                    clip.height(),
                    CombineModeIntersect,
                ) == gdip::Ok;
            let drawn = configured && paint(&Canvas(graphics));
            GdipDeleteGraphics(graphics);
            drawn
        } else {
            false
        };
        RestoreDC(dc, state);
        drawn
    }
}

impl Drop for Antialias {
    fn drop(&mut self) {
        unsafe { GdiplusShutdown(self.0) };
    }
}

impl Canvas {
    pub unsafe fn lines<'a>(
        &self,
        color: COLORREF,
        width: f32,
        mut paths: impl Iterator<Item = &'a [PointF]>,
    ) -> bool {
        self.with_pen(color, width, |pen| {
            paths.all(|path| {
                GdipDrawLines(self.0, pen, path.as_ptr(), path.len() as i32) == gdip::Ok
            })
        })
    }

    unsafe fn with_pen(
        &self,
        color: COLORREF,
        width: f32,
        draw: impl FnOnce(*mut GpPen) -> bool,
    ) -> bool {
        let mut pen = null_mut();
        if GdipCreatePen1(argb(color), width, UnitPixel, &mut pen) != gdip::Ok {
            return false;
        }
        GdipSetPenStartCap(pen, LineCapRound);
        GdipSetPenEndCap(pen, LineCapRound);
        GdipSetPenLineJoin(pen, LineJoinRound);
        let drawn = draw(pen);
        GdipDeletePen(pen);
        drawn
    }

    pub unsafe fn disc(&self, area: Area, color: COLORREF) -> bool {
        let mut brush = null_mut();
        if GdipCreateSolidFill(argb(color), &mut brush) != gdip::Ok {
            return false;
        }
        let drawn = GdipFillEllipseI(
            self.0,
            brush.cast(),
            area.left,
            area.top,
            area.width(),
            area.height(),
        ) == gdip::Ok;
        GdipDeleteBrush(brush.cast());
        drawn
    }
}

fn argb(color: COLORREF) -> u32 {
    0xff00_0000 | (color.swap_bytes() >> 8)
}

pub(super) unsafe fn paint(
    dc: HDC,
    crop: Area,
    color: COLORREF,
    antialias: Option<&Antialias>,
    paths: &[&[Point]],
) {
    if antialias.is_some_and(|renderer| {
        renderer.draw(dc, crop, |canvas| {
            // Point, GDI+ Point and Win32 POINT are all C-layout pairs of i32s.
            canvas.with_pen(color, STROKE_WIDTH as f32, |pen| {
                paths.iter().all(|path| {
                    GdipDrawLinesI(canvas.0, pen, path.as_ptr().cast(), path.len() as i32)
                        == gdip::Ok
                })
            })
        })
    }) {
        return;
    }
    let pen = CreatePen(PS_SOLID, STROKE_WIDTH, color);
    if pen.is_null() {
        return;
    }
    let previous = SelectObject(dc, pen);
    for path in paths {
        Polyline(dc, path.as_ptr().cast(), path.len() as i32);
    }
    SelectObject(dc, previous);
    DeleteObject(pen);
}
