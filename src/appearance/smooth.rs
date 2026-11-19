use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{COLORREF, RECT};
use windows_sys::Win32::Graphics::Gdi::HDC;
use windows_sys::Win32::Graphics::GdiPlus::{self as gdip, *};

pub struct GdiPlus(usize);

impl GdiPlus {
    pub fn new() -> Option<Self> {
        let mut token = 0;
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            ..Default::default()
        };
        let status = unsafe { GdiplusStartup(&mut token, &input, null_mut()) };
        (status == gdip::Ok).then(|| Self(token))
    }

    pub unsafe fn round(
        &self,
        dc: HDC,
        bounds: &RECT,
        diameter: i32,
        surface: COLORREF,
        border: COLORREF,
    ) -> bool {
        self.draw(dc, |graphics| {
            if !fill_round(graphics, bounds, diameter, border) {
                return false;
            }
            if surface == border {
                return true;
            }
            // Inset the fill by one device pixel to keep the outline inside the control.
            let interior = RECT {
                left: bounds.left + 1,
                top: bounds.top + 1,
                right: bounds.right - 1,
                bottom: bounds.bottom - 1,
            };
            fill_round(graphics, &interior, diameter - 2, surface)
        })
    }

    pub unsafe fn stroke(&self, dc: HDC, points: &[PointF], width: f32, color: COLORREF) -> bool {
        self.draw(dc, |graphics| {
            let mut pen = null_mut();
            if GdipCreatePen1(argb(color), width, UnitPixel, &mut pen) != gdip::Ok {
                return false;
            }
            GdipSetPenStartCap(pen, LineCapRound);
            GdipSetPenEndCap(pen, LineCapRound);
            GdipSetPenLineJoin(pen, LineJoinRound);
            let drawn =
                GdipDrawLines(graphics, pen, points.as_ptr(), points.len() as i32) == gdip::Ok;
            GdipDeletePen(pen);
            drawn
        })
    }

    unsafe fn draw(&self, dc: HDC, paint: impl FnOnce(*mut GpGraphics) -> bool) -> bool {
        let mut graphics = null_mut();
        if GdipCreateFromHDC(dc, &mut graphics) != gdip::Ok {
            return false;
        }
        let drawn = GdipSetPageUnit(graphics, UnitPixel) == gdip::Ok
            && GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias8x8) == gdip::Ok
            && GdipSetPixelOffsetMode(graphics, PixelOffsetModeHalf) == gdip::Ok
            && paint(graphics);
        GdipDeleteGraphics(graphics);
        drawn
    }
}

impl Drop for GdiPlus {
    fn drop(&mut self) {
        unsafe { GdiplusShutdown(self.0) };
    }
}

unsafe fn fill_round(
    graphics: *mut GpGraphics,
    bounds: &RECT,
    diameter: i32,
    color: COLORREF,
) -> bool {
    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;
    if width <= 0 || height <= 0 {
        return true;
    }

    let mut brush = null_mut();
    if GdipCreateSolidFill(argb(color), &mut brush) != gdip::Ok {
        return false;
    }
    let diameter = diameter.clamp(0, width.min(height));
    let drawn = if diameter == 0 {
        GdipFillRectangleI(
            graphics,
            brush.cast(),
            bounds.left,
            bounds.top,
            width,
            height,
        ) == gdip::Ok
    } else {
        let mut path = null_mut();
        if GdipCreatePath(FillModeAlternate, &mut path) == gdip::Ok {
            let corners = [
                (bounds.left, bounds.top, 180.0),
                (bounds.right - diameter, bounds.top, 270.0),
                (bounds.right - diameter, bounds.bottom - diameter, 0.0),
                (bounds.left, bounds.bottom - diameter, 90.0),
            ];
            let drawn = corners.into_iter().all(|(x, y, start)| {
                GdipAddPathArcI(path, x, y, diameter, diameter, start, 90.0) == gdip::Ok
            }) && GdipClosePathFigure(path) == gdip::Ok
                && GdipFillPath(graphics, brush.cast(), path) == gdip::Ok;
            GdipDeletePath(path);
            drawn
        } else {
            false
        }
    };
    GdipDeleteBrush(brush.cast());
    drawn
}

pub fn argb(color: COLORREF) -> u32 {
    0xff00_0000 | (color.swap_bytes() >> 8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{fill, PANEL, SELECTED_BORDER, WINDOW};
    use windows_sys::Win32::Graphics::Gdi::*;

    #[test]
    fn rounded_outline_blends_corners_and_keeps_straight_edges_crisp() {
        let renderer = GdiPlus::new().unwrap();
        let surface = RECT {
            left: 0,
            top: 0,
            right: 80,
            bottom: 50,
        };
        let control = RECT {
            left: 4,
            top: 4,
            right: 70,
            bottom: 37,
        };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: surface.right,
                biHeight: -surface.bottom,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };

        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            assert!(!dc.is_null());
            let mut pixels = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut pixels, null_mut(), 0);
            assert!(!bitmap.is_null());
            let previous = SelectObject(dc, bitmap);
            fill(dc, &surface, WINDOW);
            let drawn = renderer.round(dc, &control, 33, PANEL, SELECTED_BORDER);
            let straight_edge = (
                GetPixel(dc, 36, 3),
                GetPixel(dc, 36, 4),
                GetPixel(dc, 36, 5),
            );
            let corner_blends = (4..19).any(|y| {
                (4..19).any(|x| !matches!(GetPixel(dc, x, y), WINDOW | PANEL | SELECTED_BORDER))
            });
            SelectObject(dc, previous);
            DeleteObject(bitmap);
            DeleteDC(dc);
            assert!(drawn && corner_blends);
            assert_eq!(straight_edge, (WINDOW, SELECTED_BORDER, PANEL));
        }
    }
}
