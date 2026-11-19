use super::{
    annotations::AnnotationView, strokes, text, toolbar, Area, DragKind, Editor, Mark, Point,
    SELECTION_BORDER,
};
use crate::{appearance, storage::wide};
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{COLORREF, HWND, INVALID_HANDLE_VALUE, RECT};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS,
    PAGE_READWRITE,
};

const ANNOTATION_FONT_HEIGHT: i32 = 24;
const PREVIEW_TILE_WIDTH: i32 = 1024;
const PREVIEW_TILE_HEIGHT: i32 = 256;

struct Scene<'a> {
    selection: Option<Area>,
    content: Option<CropScene<'a>>,
    toolbar: Option<(toolbar::Layout, toolbar::State)>,
    hint: Option<(Area, &'static str)>,
}

struct CropScene<'a> {
    crop: Area,
    annotations: AnnotationView<'a>,
    drawing: Option<LiveDrawing<'a>>,
    draft: Option<(u64, &'a text::TextDraft)>,
    ink: COLORREF,
}

enum LiveDrawing<'a> {
    Pen(&'a strokes::LivePen),
    Shape(Mark),
}

impl LiveDrawing<'_> {
    fn bounds(&self) -> Option<Area> {
        match self {
            Self::Pen(stroke) => stroke.paint_bounds(),
            Self::Shape(mark) => Some(mark.paint_bounds()),
        }
    }
}

impl CropScene<'_> {
    fn controls(&self) -> Option<(usize, &Mark, Area)> {
        let (index, mark) = self.annotations.selected()?;
        Some((index, mark, mark.controls_bounds(self.crop.local_bounds())))
    }

    fn pen(&self) -> Option<&strokes::LivePen> {
        match &self.drawing {
            Some(LiveDrawing::Pen(stroke)) => Some(stroke),
            _ => None,
        }
    }
}

pub(super) struct PreviewSnapshot {
    selection: Option<Area>,
    toolbar: Option<(toolbar::Layout, toolbar::State)>,
    controls: Option<(usize, Area)>,
    drag: Option<Area>,
    pen: Option<(usize, COLORREF)>,
    hidden_mark: Option<Area>,
    draft: Option<(Area, COLORREF, u64)>,
    caret: Option<Area>,
    hint: Option<(Area, &'static str)>,
}

fn in_overlay(crop: Area, area: Area) -> Option<Area> {
    area.offset(crop.origin()).intersection(crop)
}

fn repaint_pair<T: Copy + PartialEq>(
    before: Option<T>,
    after: Option<T>,
    bounds: impl Fn(T) -> Area,
    repaint: &mut impl FnMut(Area),
) {
    if before != after {
        let before = before.map(&bounds);
        let after = after.map(bounds);
        before
            .into_iter()
            .chain(after.filter(|area| Some(*area) != before))
            .for_each(repaint);
    }
}

impl Scene<'_> {
    fn snapshot(&self) -> PreviewSnapshot {
        let mut preview = PreviewSnapshot {
            selection: self.selection,
            toolbar: self.toolbar,
            controls: None,
            drag: None,
            pen: None,
            hidden_mark: None,
            draft: None,
            caret: None,
            hint: self.hint,
        };
        if let Some(content) = &self.content {
            let on_screen = |area| in_overlay(content.crop, area);
            preview.controls = content
                .controls()
                .and_then(|(index, _, bounds)| Some((index, on_screen(bounds)?)));
            preview.drag = content
                .drawing
                .as_ref()
                .and_then(LiveDrawing::bounds)
                .and_then(on_screen);
            if let Some((original, changed)) = content.annotations.replacement() {
                preview.hidden_mark = on_screen(original.paint_bounds());
                preview.drag = on_screen(
                    changed
                        .paint_bounds()
                        .union(changed.controls_bounds(content.crop.local_bounds())),
                );
            }
            preview.pen = content.pen().map(|pen| (pen.points.len(), content.ink));
            if let Some((revision, draft)) = content.draft {
                preview.draft = on_screen(draft.bounds()).map(|area| (area, content.ink, revision));
                preview.caret = draft.visible_caret().and_then(on_screen);
            }
        }
        preview
    }

    fn repaint_since(&self, before: PreviewSnapshot, mut repaint: impl FnMut(Area)) {
        let after = self.snapshot();
        if before.selection != after.selection {
            let shared_interior = before
                .selection
                .zip(after.selection)
                .and_then(|(old, new)| old.padded(-1).intersection(new.padded(-1)));
            for area in before.selection.into_iter().chain(after.selection) {
                area.outside(shared_interior).for_each(&mut repaint);
            }
        }
        if before.toolbar != after.toolbar {
            for (layout, _) in before.toolbar.into_iter().chain(after.toolbar) {
                layout.panels().for_each(&mut repaint);
            }
        }
        repaint_pair(
            before.controls,
            after.controls,
            |(_, area)| area,
            &mut repaint,
        );
        repaint_pair(before.draft, after.draft, |(area, _, _)| area, &mut repaint);
        repaint_pair(before.caret, after.caret, |area| area, &mut repaint);
        repaint_pair(before.hint, after.hint, |(area, _)| area, &mut repaint);
        repaint_pair(
            before.hidden_mark,
            after.hidden_mark,
            |area| area,
            &mut repaint,
        );
        if let (Some((old, color)), Some((new, next_color))) = (before.pen, after.pen) {
            if color == next_color && new >= old {
                if new > old {
                    if let Some(area) = self.content.as_ref().and_then(|content| {
                        let damage = content.pen()?.damage_from(old)?;
                        in_overlay(content.crop, damage)
                    }) {
                        repaint(area);
                    }
                }
                return;
            }
        }
        before.drag.into_iter().chain(after.drag).for_each(repaint);
    }
}

// rcPaint is the bounding box. Keep disjoint damage disjoint when Windows provides the region.
pub(super) unsafe fn update_rectangles(hwnd: HWND) -> Option<Vec<RECT>> {
    let region = CreateRectRgn(0, 0, 0, 0);
    if region.is_null() {
        return None;
    }
    let rectangles = (|| {
        if GetUpdateRgn(hwnd, region, 0) != COMPLEXREGION {
            return None;
        }
        let size = GetRegionData(region, 0, null_mut());
        let header_size = size_of::<RGNDATAHEADER>();
        let payload = (size as usize).checked_sub(header_size)?;
        let mut data = vec![0u32; (size as usize).div_ceil(size_of::<u32>())];
        if GetRegionData(region, size, data.as_mut_ptr().cast()) != size {
            return None;
        }
        let header = &*data.as_ptr().cast::<RGNDATAHEADER>();
        if header.dwSize as usize != header_size
            || header.nCount as usize > payload / size_of::<RECT>()
        {
            return None;
        }
        let rects = data.as_ptr().cast::<u8>().add(header_size).cast::<RECT>();
        Some(std::slice::from_raw_parts(rects, header.nCount as usize).to_vec())
    })();
    DeleteObject(region);
    rectangles
}

impl Editor {
    pub(super) fn antialias(&self) -> Option<&strokes::Antialias> {
        self.antialias.get_or_init(strokes::Antialias::new).as_ref()
    }

    fn preview_selection(&self) -> Option<Area> {
        self.selection.or_else(|| match &self.drag {
            Some(drag) => self.selection_for_drag(drag),
            None => self.pointer.and_then(|point| self.window_at(point)),
        })
    }

    fn toolbar_preview(&self) -> Option<(toolbar::Layout, toolbar::State)> {
        Some((
            self.toolbar_layout()?,
            toolbar::State {
                active: self.tool,
                hovered: self.hovered,
                color: self
                    .annotations
                    .selected()
                    .map_or(self.color, |mark| mark.color),
                can_undo: self.annotations.can_undo(),
                can_redo: self.annotations.can_redo(),
            },
        ))
    }

    fn hint(&self, crop: Area) -> Option<(Area, &'static str)> {
        let message = if self
            .text
            .preview()
            .is_some_and(|(_, draft)| draft.overflows())
        {
            "文字超出选区，请缩短后再保存"
        } else if self.save_busy {
            "截图正在保存，请稍后再次确认"
        } else {
            return None;
        };
        let monitor = self.monitor_at(self.toolbar.unwrap_or(crop.origin()));
        let width = 288.min(monitor.width());
        let height = 30.min(monitor.height());
        let left = monitor.left + (monitor.width() - width) / 2;
        let top = monitor.top + 10.min(monitor.height() - height);
        let mut bounds = Area {
            left,
            top,
            right: left + width,
            bottom: top + height,
        };
        if self.toolbar_layout().is_some_and(|layout| {
            layout
                .panels()
                .any(|panel| bounds.intersection(panel).is_some())
        }) {
            bounds = bounds.offset(Point {
                x: 0,
                y: monitor.bottom - height - top,
            });
        }
        Some((bounds, message))
    }

    fn scene(&self) -> Scene<'_> {
        let selection = self.preview_selection();
        let content = self.selection.map(|crop| {
            let mut transform = None;
            let mut drawing = None;
            if let Some(drag) = &self.drag {
                match &drag.kind {
                    DragKind::Mark(mark) => transform = Some((*mark, drag.delta())),
                    DragKind::Pen(stroke) => drawing = Some(LiveDrawing::Pen(stroke)),
                    DragKind::Draw(_) => {
                        drawing = drag.drawn_mark(crop, self.color).map(LiveDrawing::Shape)
                    }
                    _ => {}
                }
            }
            CropScene {
                crop,
                annotations: self.annotations.view(transform, crop.local_bounds()),
                drawing,
                draft: self.text.preview(),
                ink: self.color,
            }
        });
        Scene {
            selection,
            content,
            toolbar: self.toolbar_preview(),
            hint: self.selection.and_then(|crop| self.hint(crop)),
        }
    }

    pub(super) fn preview_snapshot(&self) -> PreviewSnapshot {
        self.scene().snapshot()
    }

    pub(super) fn repaint_changes(
        &mut self,
        before: PreviewSnapshot,
        mut repaint: impl FnMut(Area),
    ) {
        let desktop = self.desktop.local_bounds();
        let mut repaint = |area: Area| {
            if let Some(visible) = area.intersection(desktop) {
                repaint(visible);
            }
        };
        for area in self.annotations.drain_changed_bounds() {
            if let Some(visible) = self.selection.and_then(|crop| in_overlay(crop, area)) {
                repaint(visible);
            }
        }
        self.scene().repaint_since(before, repaint);
    }

    pub(super) fn paint(&mut self, destination: HDC, rectangles: &[RECT]) {
        let Some(mut frame) = self.frame.take() else {
            return;
        };
        let scene = self.scene();
        for rect in rectangles {
            let Some(dirty) = Area::from_rect(*rect).intersection(self.desktop.local_bounds())
            else {
                continue;
            };
            let mut top = dirty.top;
            while top < dirty.bottom {
                let bottom = (top + frame.height).min(dirty.bottom);
                let mut left = dirty.left;
                while left < dirty.right {
                    let right = (left + frame.width).min(dirty.right);
                    let tile = Area {
                        left,
                        top,
                        right,
                        bottom,
                    };
                    self.compose_frame(&mut frame, tile, &scene);
                    unsafe {
                        BitBlt(
                            destination,
                            left,
                            top,
                            tile.width(),
                            tile.height(),
                            frame.dc,
                            0,
                            0,
                            SRCCOPY,
                        );
                    }
                    left = right;
                }
                top = bottom;
            }
        }
        self.frame = Some(frame);
    }

    fn compose_frame(&self, frame: &mut Bitmap, dirty: Area, scene: &Scene<'_>) {
        let dc = frame.dc;
        let selection = scene.selection;
        self.paint_background(frame, dirty, selection);
        unsafe {
            let Some(crop) = selection else { return };
            let state = SaveDC(dc);
            if state == 0 {
                return;
            }
            SetViewportOrgEx(dc, -dirty.left, -dirty.top, null_mut());
            IntersectClipRect(dc, dirty.left, dirty.top, dirty.right, dirty.bottom);
            if let Some(visible) = crop.intersection(dirty) {
                if let Some(content) = &scene.content {
                    self.paint_selection(dc, content, visible);
                }
                if visible.intersection(crop.padded(-1)) != Some(visible) {
                    let brush = CreateSolidBrush(SELECTION_BORDER);
                    if !brush.is_null() {
                        FrameRect(dc, &crop.rect(), brush);
                        DeleteObject(brush);
                    }
                }
            }
            if let Some((layout, state)) = scene.toolbar {
                if layout
                    .panels()
                    .any(|panel| panel.intersection(dirty).is_some())
                {
                    layout.paint(dc, state, self.antialias());
                }
            }
            if let Some((hint, message)) = scene
                .hint
                .filter(|(area, _)| area.intersection(dirty).is_some())
            {
                appearance::round(dc, &hint.rect(), 8, text::OVERFLOW, text::OVERFLOW);
                appearance::label(
                    dc,
                    message,
                    hint.rect(),
                    appearance::WHITE,
                    DT_CENTER | DT_END_ELLIPSIS,
                );
            }
            RestoreDC(dc, state);
        }
    }

    fn paint_background(&self, frame: &mut Bitmap, dirty: Area, selection: Option<Area>) {
        let mut shade = dirty.outside(selection).peekable();
        if shade.peek().is_some() {
            // Complete prior GDI access before writing the DIB pixels directly.
            unsafe { GdiFlush() };
            let source_stride = self.original.width as usize * 4;
            let frame_stride = frame.width as usize * 4;
            let original = self.original.bytes();
            let frame = frame.bytes_mut();
            let darken = |channel| (u16::from(channel) * 43 / 100) as u8;
            for area in shade {
                for y in area.top as usize..area.bottom as usize {
                    let source = y * source_stride + area.left as usize * 4;
                    let target = (y - dirty.top as usize) * frame_stride
                        + (area.left - dirty.left) as usize * 4;
                    let length = area.width() as usize * 4;
                    for (source, pixel) in original[source..source + length]
                        .chunks_exact(4)
                        .zip(frame[target..target + length].chunks_exact_mut(4))
                    {
                        pixel.copy_from_slice(&[
                            darken(source[0]),
                            darken(source[1]),
                            darken(source[2]),
                            0,
                        ]);
                    }
                }
            }
        }
        if let Some(visible) = selection.and_then(|crop| crop.intersection(dirty)) {
            unsafe {
                BitBlt(
                    frame.dc,
                    visible.left - dirty.left,
                    visible.top - dirty.top,
                    visible.width(),
                    visible.height(),
                    self.original.dc,
                    visible.left,
                    visible.top,
                    SRCCOPY,
                );
            }
        }
    }

    unsafe fn paint_selection(&self, dc: HDC, content: &CropScene<'_>, visible: Area) {
        let state = SaveDC(dc);
        if state == 0 {
            return;
        }
        let crop = content.crop;
        let local = crop.local_bounds();
        OffsetViewportOrgEx(dc, crop.left, crop.top, null_mut());
        IntersectClipRect(dc, local.left, local.top, local.right, local.bottom);
        let visible = visible.offset(Point {
            x: -crop.left,
            y: -crop.top,
        });
        let font = self.text.font();
        let paint = |mark: &Mark| {
            if mark.paint_bounds().intersection(visible).is_some() {
                mark.paint(dc, font, local, self.antialias());
            }
        };
        content.annotations.marks().for_each(paint);
        match &content.drawing {
            Some(LiveDrawing::Pen(stroke))
                if stroke
                    .paint_bounds()
                    .is_some_and(|bounds| bounds.intersection(visible).is_some()) =>
            {
                if let Some(points) = stroke.visible_points(visible) {
                    strokes::paint(dc, local, content.ink, self.antialias(), &[points]);
                }
            }
            Some(LiveDrawing::Shape(mark)) => paint(mark),
            _ => {}
        }
        if let Some((_, mark, bounds)) = content.controls() {
            if bounds.intersection(visible).is_some() {
                mark.paint_controls(dc, local);
            }
        }
        if let (Some((_, draft)), Some(font)) = (content.draft, font) {
            if draft.bounds().intersection(visible).is_some()
                || draft
                    .visible_caret()
                    .is_some_and(|caret| caret.intersection(visible).is_some())
            {
                draft.paint(dc, font, content.ink);
            }
        }
        RestoreDC(dc, state);
    }
}

pub struct Image {
    pub width: u32,
    pub height: u32,
    stride: usize,
    offset: usize,
    pixels: Pixels,
}

enum Pixels {
    Owned(Vec<u8>),
    Mapped { handle: OwnedHandle, length: usize },
}

struct ReadView(MEMORY_MAPPED_VIEW_ADDRESS);

impl Drop for ReadView {
    fn drop(&mut self) {
        unsafe { UnmapViewOfFile(self.0) };
    }
}

impl Image {
    pub fn byte_len(&self) -> usize {
        match &self.pixels {
            Pixels::Owned(bytes) => bytes.len(),
            Pixels::Mapped { length, .. } => *length,
        }
    }

    pub fn with_rows<T>(
        &self,
        read: impl FnOnce(&mut dyn Iterator<Item = &[u8]>) -> io::Result<T>,
    ) -> io::Result<T> {
        let read_rows = |bytes: &[u8]| {
            let row_bytes = self.width as usize * 4;
            let mut rows = bytes[self.offset..]
                .chunks(self.stride)
                .take(self.height as usize)
                .map(|row| &row[..row_bytes]);
            read(&mut rows)
        };
        match &self.pixels {
            Pixels::Owned(bytes) => read_rows(bytes),
            Pixels::Mapped { handle, length } => {
                let view =
                    unsafe { MapViewOfFile(handle.as_raw_handle(), FILE_MAP_READ, 0, 0, *length) };
                if view.Value.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let view = ReadView(view);
                let bytes = unsafe { std::slice::from_raw_parts(view.0.Value.cast(), *length) };
                read_rows(bytes)
            }
        }
    }

    pub(crate) fn from_bgra(width: u32, height: u32, bytes: Vec<u8>) -> Self {
        assert_eq!(
            bitmap_length(width as i32, height as i32).ok(),
            Some(bytes.len())
        );
        Self {
            width,
            height,
            stride: width as usize * 4,
            offset: 0,
            pixels: Pixels::Owned(bytes),
        }
    }

    #[cfg(test)]
    pub(super) fn rgb(&self) -> Vec<u8> {
        self.with_rows(|rows| {
            Ok(rows
                .flat_map(|row| row.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]))
                .collect())
        })
        .unwrap()
    }
}

impl Editor {
    pub(super) fn exceeds_save_budget(&self, budget: usize) -> bool {
        self.selection
            .and_then(|area| bitmap_length(area.width(), area.height()).ok())
            .is_some_and(|bytes| bytes > budget)
    }

    pub(super) fn export(mut self, save_budget: usize) -> io::Result<Image> {
        drop(self.frame.take());
        let selection = self
            .selection
            .ok_or_else(|| io::Error::other("未选择截图区域"))?;
        let width = selection.width();
        let height = selection.height();
        let length = bitmap_length(width, height)?;
        if length > save_budget {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "截图保存队列已满，请等待上一张截图保存后重试",
            ));
        }
        let stride = self.original.width as usize * 4;
        let offset = selection.top as usize * stride + selection.left as usize * 4;
        // Keep the original mapping for large crops. Copy small crops so a queued
        // image does not retain the entire desktop.
        if self.original.length / 2 <= length && self.original.length <= save_budget {
            if !self.annotations.marks().is_empty() {
                self.paint_export_marks(self.original.dc, selection, selection.origin())?;
            }
            return Ok(self.original.into_image(width, height, offset));
        }
        if self.annotations.marks().is_empty() {
            let mut bytes = Vec::with_capacity(length);
            for row in self.original.bytes()[offset..]
                .chunks(stride)
                .take(height as usize)
            {
                bytes.extend_from_slice(&row[..width as usize * 4]);
            }
            return Ok(Image::from_bgra(width as u32, height as u32, bytes));
        }
        self.render_annotated_image(selection)
    }

    pub(super) fn render_annotated_image(&self, selection: Area) -> io::Result<Image> {
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
        }
        self.paint_export_marks(image.dc, selection, Point { x: 0, y: 0 })?;
        Ok(image.into_image(width, height, 0))
    }

    fn paint_export_marks(&self, dc: HDC, selection: Area, origin: Point) -> io::Result<()> {
        let crop = selection.local_bounds();
        unsafe {
            let state = SaveDC(dc);
            if state == 0 {
                return Err(io::Error::last_os_error());
            }
            SetViewportOrgEx(dc, origin.x, origin.y, null_mut());
            IntersectClipRect(dc, 0, 0, crop.width(), crop.height());
            for mark in self.annotations.marks() {
                mark.paint(dc, self.text.font(), crop, self.antialias());
            }
            RestoreDC(dc, state);
            GdiFlush();
        }
        Ok(())
    }
}

pub(super) struct Font {
    pub(super) handle: HFONT,
    pub(super) chinese_width: i32,
    pub(super) line_height: i32,
}

impl Font {
    pub(super) fn new(dc: HDC) -> io::Result<Self> {
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

pub(super) struct Bitmap {
    pub(super) dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    length: usize,
    width: i32,
    height: i32,
    mapping: Option<OwnedHandle>,
}

fn bitmap_length(width: i32, height: i32) -> io::Result<usize> {
    (i64::from(width) * i64::from(height))
        .checked_mul(4)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .filter(|&length| width > 0 && height > 0 && length <= isize::MAX as usize)
        .ok_or_else(|| io::Error::other("屏幕尺寸无效"))
}

impl Bitmap {
    pub(super) fn preview(desktop: Area) -> io::Result<Self> {
        Self::new(
            desktop.width().min(PREVIEW_TILE_WIDTH),
            desktop.height().min(PREVIEW_TILE_HEIGHT),
        )
    }

    pub(super) fn new(width: i32, height: i32) -> io::Result<Self> {
        let length = bitmap_length(width, height)?;
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
            // Paging-file mappings start zeroed, including gaps between displays.
            let mapping = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                null(),
                PAGE_READWRITE,
                (length as u64 >> 32) as u32,
                length as u32,
                null(),
            );
            if mapping.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mapping = OwnedHandle::from_raw_handle(mapping);
            let dc = CreateCompatibleDC(null_mut());
            if dc.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut bits = null_mut();
            let bitmap = CreateDIBSection(
                dc,
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                mapping.as_raw_handle(),
                0,
            );
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
            Ok(Self {
                dc,
                bitmap,
                previous,
                bits: bits.cast(),
                length,
                width,
                height,
                mapping: Some(mapping),
            })
        }
    }

    fn into_image(mut self, width: i32, height: i32, offset: usize) -> Image {
        // Only the memory mapping crosses threads. Dropping self releases the UI thread's DC.
        let handle = self.mapping.take().unwrap();
        Image {
            width: width as u32,
            height: height as u32,
            stride: self.width as usize * 4,
            offset,
            pixels: Pixels::Mapped {
                handle,
                length: self.length,
            },
        }
    }

    pub(super) fn capture(desktop: Area, monitors: &[Area]) -> io::Result<Self> {
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

    pub(super) fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.bits, self.length) }
    }
    pub(super) fn bytes_mut(&mut self) -> &mut [u8] {
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
