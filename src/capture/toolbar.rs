use super::{strokes, Area, Point, Tool, INK};
use crate::appearance::{
    fill, primary_fill, rgb, round, selected_fill, ACCENT, ACCENT_TEXT, BORDER, CONTROL_BORDER,
    CONTROL_CORNER, HOVER, MUTED, PANEL, PANEL_CORNER, SELECTED_BORDER, SELECTED_TEXT, TEXT,
};
use std::ptr::{null, null_mut};
use windows_sys::core::{w, PCWSTR};
use windows_sys::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, POINT};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, SendMessageW, CW_USEDEFAULT, WS_EX_NOACTIVATE, WS_EX_TOPMOST,
    WS_POPUP,
};

const PADDING: i32 = 6;
const GAP: i32 = 3;
const BUTTON_SIZE: i32 = 44;
const GRIP_WIDTH: i32 = 28;
const ICON_SIZE: i32 = 22;
const ICON_LINE_WIDTH: f32 = 2.5;
const PALETTE_SIZE: i32 = 34;
const PALETTE_HEIGHT: i32 = PALETTE_SIZE + PADDING * 2;
const POPOVER_GAP: i32 = 6;
pub(super) const HEIGHT: i32 = BUTTON_SIZE + PADDING * 2;
pub(super) const WIDTH: i32 = 2 * PADDING
    + GAP * (BUTTONS.len() as i32 - 1)
    + BUTTON_SIZE * (BUTTONS.len() as i32 - 1)
    + GRIP_WIDTH;

const COLORS: [COLORREF; 7] = [
    INK,
    rgb(245, 147, 38),
    rgb(247, 212, 55),
    rgb(24, 170, 120),
    rgb(43, 123, 246),
    rgb(245, 247, 250),
    rgb(27, 29, 33),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Button {
    Grip,
    Tool(Tool),
    Palette,
    Swatch(COLORREF),
    Undo,
    Redo,
    Cancel,
    Save,
}

impl Button {
    pub fn tooltip_text(self) -> Option<PCWSTR> {
        Some(match self {
            Self::Grip => w!("拖动工具栏"),
            Self::Tool(Tool::Select) => w!("选择标注"),
            Self::Tool(Tool::Rectangle) => w!("画矩形"),
            Self::Tool(Tool::Arrow) => w!("画箭头"),
            Self::Tool(Tool::Pen) => w!("画笔"),
            Self::Tool(Tool::Text) => w!("添加文字"),
            Self::Tool(Tool::Eraser) => w!("擦除标注"),
            Self::Palette => w!("选择颜色"),
            Self::Undo => w!("撤销"),
            Self::Redo => w!("重做"),
            Self::Cancel => w!("取消截图"),
            Self::Save => w!("保存截图"),
            Self::Swatch(_) => return None,
        })
    }
}

const BUTTONS: [Button; 12] = [
    Button::Grip,
    Button::Tool(Tool::Select),
    Button::Tool(Tool::Rectangle),
    Button::Tool(Tool::Arrow),
    Button::Tool(Tool::Pen),
    Button::Tool(Tool::Text),
    Button::Palette,
    Button::Tool(Tool::Eraser),
    Button::Undo,
    Button::Redo,
    Button::Cancel,
    Button::Save,
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Layout {
    pub origin: Point,
    pub monitor: Area,
    pub palette_open: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct State {
    pub active: Tool,
    pub hovered: Option<Button>,
    pub color: COLORREF,
    pub can_undo: bool,
    pub can_redo: bool,
}

pub(super) struct Tooltip {
    hwnd: HWND,
    tool: TTTOOLINFOW,
}

impl Tooltip {
    pub fn new(overlay: HWND, instance: HINSTANCE) -> Option<Self> {
        unsafe {
            if InitCommonControlsEx(&INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_BAR_CLASSES,
            }) == 0
            {
                return None;
            }
            // The overlay owns the popup and Windows destroys it with the overlay.
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                TOOLTIPS_CLASSW,
                null(),
                WS_POPUP | TTS_NOPREFIX | TTS_ALWAYSTIP,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                overlay,
                null_mut(),
                instance,
                null(),
            );
            if hwnd.is_null() {
                return None;
            }
            let tool = TTTOOLINFOW {
                cbSize: size_of::<TTTOOLINFOW>() as u32,
                uFlags: TTF_TRACK | TTF_ABSOLUTE | TTF_TRANSPARENT,
                hwnd: overlay,
                uId: 1,
                lpszText: w!("").cast_mut(),
                ..Default::default()
            };
            if SendMessageW(hwnd, TTM_ADDTOOLW, 0, &tool as *const _ as LPARAM) == 0 {
                DestroyWindow(hwnd);
                return None;
            }
            Some(Self { hwnd, tool })
        }
    }

    pub fn show(&mut self, layout: Layout, button: Button, desktop_origin: Point) {
        let Some(text) = button.tooltip_text() else {
            return;
        };
        self.tool.lpszText = text.cast_mut();
        let tool = &self.tool as *const _ as LPARAM;
        unsafe {
            SendMessageW(self.hwnd, TTM_UPDATETIPTEXTW, 0, tool);
            let size = SendMessageW(self.hwnd, TTM_GETBUBBLESIZE, 0, tool) as u32;
            let width = i32::from(size as u16);
            let height = i32::from((size >> 16) as u16);
            if let Some(point) = layout.tooltip_origin(button, width, height) {
                let screen = point + desktop_origin;
                let position = u32::from(screen.x as u16) | (u32::from(screen.y as u16) << 16);
                SendMessageW(self.hwnd, TTM_TRACKPOSITION, 0, position as LPARAM);
                SendMessageW(self.hwnd, TTM_TRACKACTIVATE, 1, tool);
            }
        }
    }

    pub fn hide(&self) {
        unsafe {
            SendMessageW(
                self.hwnd,
                TTM_TRACKACTIVATE,
                0,
                &self.tool as *const _ as LPARAM,
            )
        };
    }
}

enum Glyph {
    Symbol(Button, Area, COLORREF),
    Swatch(Area, COLORREF, COLORREF),
}

impl Layout {
    pub fn panel(&self) -> Area {
        area_at(self.origin, WIDTH, HEIGHT)
    }

    pub fn panels(&self) -> impl Iterator<Item = Area> + '_ {
        std::iter::once(self.panel()).chain(self.palette())
    }

    pub fn buttons(&self) -> impl Iterator<Item = (Button, Area)> {
        let mut x = self.origin.x + PADDING;
        let y = self.origin.y + PADDING;
        BUTTONS.into_iter().map(move |button| {
            let width = if button == Button::Grip {
                GRIP_WIDTH
            } else {
                BUTTON_SIZE
            };
            let area = area_at(Point { x, y }, width, BUTTON_SIZE);
            x += width + GAP;
            (button, area)
        })
    }

    fn palette(&self) -> Option<Area> {
        if !self.palette_open {
            return None;
        }
        let (_, anchor) = self
            .buttons()
            .find(|(button, _)| *button == Button::Palette)?;
        let width = COLORS.len() as i32 * PALETTE_SIZE + 2 * PADDING;
        let preferred = anchor.left + anchor.width() / 2 - width / 2;
        let x = preferred.clamp(
            self.monitor.left,
            (self.monitor.right - width).max(self.monitor.left),
        );
        let top = self.origin.y - PALETTE_HEIGHT - POPOVER_GAP;
        let y = if top >= self.monitor.top {
            top
        } else {
            self.origin.y + HEIGHT + POPOVER_GAP
        };
        Some(area_at(Point { x, y }, width, PALETTE_HEIGHT))
    }

    fn swatches(panel: Area) -> impl Iterator<Item = (Button, Area)> {
        COLORS.into_iter().enumerate().map(move |(index, color)| {
            let point = Point {
                x: panel.left + PADDING + index as i32 * PALETTE_SIZE,
                y: panel.top + PADDING,
            };
            (
                Button::Swatch(color),
                area_at(point, PALETTE_SIZE, PALETTE_SIZE),
            )
        })
    }

    pub fn hit(&self, point: Point) -> Option<Button> {
        self.palette()
            .into_iter()
            .flat_map(Self::swatches)
            .chain(self.buttons())
            .find(|(_, area)| area.contains(point))
            .map(|(button, _)| button)
    }

    fn tooltip_origin(&self, button: Button, width: i32, height: i32) -> Option<Point> {
        let (_, anchor) = self.buttons().find(|(candidate, _)| *candidate == button)?;
        let x = (anchor.left + (anchor.width() - width) / 2).clamp(
            self.monitor.left,
            (self.monitor.right - width).max(self.monitor.left),
        );
        let panel = self.panel();
        let palette = self.palette();
        let below = |area: Area| area.bottom + POPOVER_GAP;
        let above = |area: Area| area.top - POPOVER_GAP - height;
        [
            Some(below(panel)),
            palette.map(below),
            Some(above(panel)),
            palette.map(above),
        ]
        .into_iter()
        .flatten()
        .map(|y| area_at(Point { x, y }, width, height))
        .find(|bounds| {
            self.monitor.intersection(*bounds) == Some(*bounds)
                && bounds.intersection(panel.padded(POPOVER_GAP)).is_none()
                && palette
                    .is_none_or(|area| bounds.intersection(area.padded(POPOVER_GAP)).is_none())
        })
        .map(Area::origin)
    }

    pub fn contains(&self, point: Point) -> bool {
        self.panel().contains(point) || self.palette().is_some_and(|area| area.contains(point))
    }

    pub unsafe fn paint(&self, dc: HDC, state: State, antialias: Option<&strokes::Antialias>) {
        let mut glyphs = Vec::with_capacity(BUTTONS.len() + COLORS.len());
        round(dc, &self.panel().rect(), PANEL_CORNER, PANEL, BORDER);
        for (button, area) in self.buttons() {
            if matches!(button, Button::Palette | Button::Undo | Button::Cancel) {
                let rule = area_at(
                    Point {
                        x: area.left - GAP,
                        y: area.top + 7,
                    },
                    1,
                    BUTTON_SIZE - 14,
                );
                fill(dc, &rule.rect(), BORDER);
            }
            let hovered = state.hovered == Some(button);
            let selected = button == Button::Tool(state.active)
                || (button == Button::Palette && self.palette_open);
            let unavailable = (button == Button::Undo && !state.can_undo)
                || (button == Button::Redo && !state.can_redo);
            let (background, border, foreground) = if button == Button::Save {
                let background = primary_fill(hovered, false);
                (background, background, ACCENT_TEXT)
            } else if selected {
                (selected_fill(hovered), SELECTED_BORDER, SELECTED_TEXT)
            } else if hovered && !unavailable {
                (HOVER, CONTROL_BORDER, TEXT)
            } else {
                (
                    PANEL,
                    PANEL,
                    if unavailable || button == Button::Grip {
                        MUTED
                    } else {
                        TEXT
                    },
                )
            };
            if background != PANEL {
                round(dc, &area.rect(), CONTROL_CORNER, background, border);
            }
            if button == Button::Palette {
                glyphs.push(Glyph::Swatch(
                    swatch_bounds(area, 9),
                    state.color,
                    foreground,
                ));
            } else {
                glyphs.push(Glyph::Symbol(button, area, foreground));
            }
        }
        if let Some(palette) = self.palette() {
            round(dc, &palette.rect(), PANEL_CORNER, PANEL, BORDER);
            for (button, area) in Self::swatches(palette) {
                let Button::Swatch(color) = button else {
                    unreachable!()
                };
                let hovered = state.hovered == Some(button);
                let selected = color == state.color;
                if selected || hovered {
                    let (surface, border) = if selected {
                        (selected_fill(hovered), SELECTED_BORDER)
                    } else {
                        (HOVER, CONTROL_BORDER)
                    };
                    round(dc, &area.rect(), CONTROL_CORNER, surface, border);
                }
                let outline = if selected { ACCENT } else { MUTED };
                glyphs.push(Glyph::Swatch(swatch_bounds(area, 10), color, outline));
            }
        }
        // Keep the GDI backgrounds and the GDI+ icon layer in separate passes.
        let drawn = antialias.is_some_and(|renderer| {
            renderer.draw(dc, self.monitor, |canvas| {
                glyphs.iter().all(|glyph| glyph.paint(dc, Some(canvas)))
            })
        });
        if !drawn {
            for glyph in &glyphs {
                glyph.paint(dc, None);
            }
        }
    }
}

fn area_at(point: Point, width: i32, height: i32) -> Area {
    Area {
        left: point.x,
        top: point.y,
        right: point.x + width,
        bottom: point.y + height,
    }
}

fn swatch_bounds(area: Area, radius: i32) -> Area {
    let x = area.left + area.width() / 2;
    let y = area.top + area.height() / 2;
    area_at(
        Point {
            x: x - radius,
            y: y - radius,
        },
        radius * 2,
        radius * 2,
    )
}

impl Glyph {
    unsafe fn paint(&self, dc: HDC, canvas: Option<&strokes::Canvas>) -> bool {
        match *self {
            Self::Symbol(button, area, color) => paint_icon(dc, canvas, button, area, color),
            Self::Swatch(area, color, outline) => {
                if let Some(canvas) = canvas {
                    let inner = Area {
                        left: area.left + 1,
                        top: area.top + 1,
                        right: area.right - 1,
                        bottom: area.bottom - 1,
                    };
                    canvas.disc(area, outline) && canvas.disc(inner, color)
                } else {
                    round(dc, &area.rect(), area.width(), color, outline);
                    true
                }
            }
        }
    }
}

unsafe fn paint_icon(
    dc: HDC,
    canvas: Option<&strokes::Canvas>,
    button: Button,
    area: Area,
    color: COLORREF,
) -> bool {
    let origin = Point {
        x: area.left + (area.width() - ICON_SIZE) / 2,
        y: area.top + (area.height() - ICON_SIZE) / 2,
    };
    let scale = |units: i32| units as f32 * ICON_SIZE as f32 / 16.0;
    if button == Button::Grip {
        return [3, 7, 11].into_iter().all(|y| {
            [5, 9].into_iter().all(|x| {
                let dot = area_at(
                    origin
                        + Point {
                            x: scale(x).round() as i32,
                            y: scale(y).round() as i32,
                        },
                    3,
                    3,
                );
                if let Some(canvas) = canvas {
                    canvas.disc(dot, color)
                } else {
                    fill(dc, &dot.rect(), color);
                    true
                }
            })
        });
    }
    let geometry = icon_paths(button);
    if let Some(canvas) = canvas {
        let paths: Vec<Vec<_>> = geometry
            .iter()
            .map(|line| {
                line.iter()
                    .map(|&(x, y)| strokes::PointF {
                        X: origin.x as f32 + scale(x),
                        Y: origin.y as f32 + scale(y),
                    })
                    .collect()
            })
            .collect();
        return canvas.lines(color, ICON_LINE_WIDTH, paths.iter().map(Vec::as_slice));
    }
    let pen = CreatePen(PS_SOLID, ICON_LINE_WIDTH.round() as i32, color);
    if pen.is_null() {
        return false;
    }
    let previous = SelectObject(dc, pen);
    for line in geometry {
        let points: Vec<_> = line
            .iter()
            .map(|&(x, y)| POINT {
                x: origin.x + scale(x).round() as i32,
                y: origin.y + scale(y).round() as i32,
            })
            .collect();
        Polyline(dc, points.as_ptr(), points.len() as i32);
    }
    SelectObject(dc, previous);
    DeleteObject(pen);
    true
}

type IconPath = &'static [(i32, i32)];

fn icon_paths(button: Button) -> &'static [IconPath] {
    match button {
        Button::Tool(Tool::Select) => &[&[
            (3, 1),
            (3, 13),
            (6, 10),
            (9, 15),
            (11, 14),
            (8, 9),
            (13, 9),
            (3, 1),
        ]],
        Button::Tool(Tool::Rectangle) => &[&[(1, 3), (15, 3), (15, 13), (1, 13), (1, 3)]],
        Button::Tool(Tool::Arrow) => &[&[(2, 14), (14, 2)], &[(7, 2), (14, 2), (14, 9)]],
        Button::Tool(Tool::Pen) => {
            &[&[(2, 13), (5, 6), (7, 5), (8, 11), (10, 12), (12, 7), (15, 5)]]
        }
        Button::Tool(Tool::Text) => &[&[(2, 3), (14, 3)], &[(8, 3), (8, 14)], &[(5, 14), (11, 14)]],
        Button::Tool(Tool::Eraser) => &[
            &[(1, 10), (9, 2), (15, 8), (9, 14), (5, 14), (1, 10)],
            &[(5, 6), (12, 12)],
        ],
        Button::Undo => &[
            &[(1, 6), (10, 6), (13, 8), (13, 12), (11, 14), (7, 14)],
            &[(5, 2), (1, 6), (5, 10)],
        ],
        Button::Redo => &[
            &[(15, 6), (6, 6), (3, 8), (3, 12), (5, 14), (9, 14)],
            &[(11, 2), (15, 6), (11, 10)],
        ],
        Button::Cancel => &[&[(3, 3), (13, 13)], &[(13, 3), (3, 13)]],
        Button::Save => &[&[(2, 8), (6, 12), (14, 4)]],
        Button::Grip | Button::Palette | Button::Swatch(_) => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Bitmap;
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsWindow, IsWindowVisible};

    #[test]
    fn native_tooltip_opens_below_the_button_with_room_and_hides() {
        let instance = unsafe { GetModuleHandleW(null()) };
        let overlay = unsafe {
            CreateWindowExW(
                0,
                w!("STATIC"),
                null(),
                WS_POPUP,
                0,
                0,
                WIDTH + 40,
                HEIGHT + 100,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        assert!(!overlay.is_null());
        let mut tooltip = Tooltip::new(overlay, instance).unwrap();
        let layout = Layout {
            origin: Point { x: 12, y: 10 },
            monitor: area_at(Point { x: 0, y: 0 }, WIDTH + 40, HEIGHT + 100),
            palette_open: false,
        };
        let button = Button::Tool(Tool::Rectangle);
        tooltip.show(layout, button, Point { x: 0, y: 0 });
        unsafe {
            let visible = IsWindowVisible(tooltip.hwnd);
            let mut rect = RECT::default();
            let positioned = GetWindowRect(tooltip.hwnd, &mut rect);
            tooltip.hide();
            let hidden = IsWindowVisible(tooltip.hwnd) == 0;
            DestroyWindow(overlay);
            assert_ne!(visible, 0);
            assert_ne!(positioned, 0);
            assert_eq!(rect.top, layout.panel().bottom + POPOVER_GAP);
            assert!(rect.bottom > rect.top);
            assert!(hidden);
            assert_eq!(IsWindow(tooltip.hwnd), 0);
        }
    }

    #[test]
    fn toolbar_icons_blend_edges_into_panel() {
        let monitor = area_at(Point { x: 0, y: 0 }, WIDTH + 40, HEIGHT + 40);
        let layout = Layout {
            origin: Point { x: 12, y: 10 },
            monitor,
            palette_open: false,
        };
        let bitmap = Bitmap::new(monitor.width(), monitor.height()).unwrap();
        let antialias = strokes::Antialias::new().unwrap();
        unsafe {
            layout.paint(
                bitmap.dc,
                State {
                    active: Tool::Rectangle,
                    hovered: None,
                    color: INK,
                    can_undo: false,
                    can_redo: false,
                },
                Some(&antialias),
            );
            GdiFlush();
        }
        for (button, foreground) in [
            (Button::Grip, MUTED),
            (Button::Tool(Tool::Arrow), TEXT),
            (Button::Tool(Tool::Pen), TEXT),
            (Button::Palette, TEXT),
            (Button::Cancel, TEXT),
        ] {
            let (_, area) = layout
                .buttons()
                .find(|(candidate, _)| *candidate == button)
                .unwrap();
            let center = Point {
                x: area.left + area.width() / 2,
                y: area.top + area.height() / 2,
            };
            let has_blended_pixel = (center.y - 11..center.y + 11).any(|y| {
                (center.x - 11..center.x + 11).any(|x| {
                    let index = (y * monitor.width() + x) as usize * 4;
                    let red = u32::from(bitmap.bytes()[index + 2]);
                    (PANEL & 0xff) < red && red < (foreground & 0xff)
                })
            });
            assert!(has_blended_pixel, "no antialiasing in {button:?}");
        }
    }
}
