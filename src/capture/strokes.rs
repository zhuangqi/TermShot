use super::{Area, Point};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{COLORREF, POINT};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Graphics::GdiPlus::{self as gdip, *};

pub(super) use gdip::PointF;

const STROKE_WIDTH: i32 = 3;

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
        // Give GDI+ device coordinates even when the GDI viewport has a crop offset.
        let state = SaveDC(dc);
        if state == 0 {
            return false;
        }
        SetViewportOrgEx(dc, 0, 0, null_mut());
        let mut graphics = null_mut();
        let drawn = if GdipCreateFromHDC(dc, &mut graphics) == gdip::Ok {
            let configured = GdipSetPageUnit(graphics, UnitPixel) == gdip::Ok
                && GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias8x8) == gdip::Ok
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
        let mut pen = null_mut();
        if GdipCreatePen1(argb(color), width, UnitPixel, &mut pen) != gdip::Ok {
            return false;
        }
        GdipSetPenStartCap(pen, LineCapRound);
        GdipSetPenEndCap(pen, LineCapRound);
        GdipSetPenLineJoin(pen, LineJoinRound);
        let drawn = paths
            .all(|path| GdipDrawLines(self.0, pen, path.as_ptr(), path.len() as i32) == gdip::Ok);
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
            let paths: Vec<Vec<_>> = paths
                .iter()
                .map(|path| {
                    path.iter()
                        .map(|point| PointF {
                            X: (point.x + crop.left) as f32,
                            Y: (point.y + crop.top) as f32,
                        })
                        .collect()
                })
                .collect();
            canvas.lines(color, STROKE_WIDTH as f32, paths.iter().map(Vec::as_slice))
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
        let points: Vec<_> = path
            .iter()
            .map(|point| POINT {
                x: point.x,
                y: point.y,
            })
            .collect();
        Polyline(dc, points.as_ptr(), points.len() as i32);
    }
    SelectObject(dc, previous);
    DeleteObject(pen);
}
