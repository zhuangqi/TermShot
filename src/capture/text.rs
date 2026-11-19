use super::{Area, Font, Point, SELECTION_BORDER};
use crate::appearance;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Input::Ime::IMECHARPOSITION;

mod native;
use native::NativeEdit;
pub(super) use native::{handle_finish_key, CARET_TIMER, FINISHED, IME_POSITION, UPDATED};

pub(super) const OVERFLOW: COLORREF = appearance::rgb(177, 34, 42);
const INPUT_PADDING: i32 = 5;
const CARET_WIDTH: i32 = 1;

#[derive(Clone, Copy)]
struct TextMetrics<'a> {
    chinese_width: i32,
    line_height: i32,
    measure: &'a dyn Fn(&[u16]) -> i32,
}

fn with_metrics<T>(dc: HDC, font: &Font, action: impl FnOnce(TextMetrics<'_>) -> T) -> T {
    let previous = unsafe { SelectObject(dc, font.handle) };
    let result = action(TextMetrics {
        chinese_width: font.chinese_width,
        line_height: font.line_height,
        measure: &|text| measure(dc, text),
    });
    unsafe { SelectObject(dc, previous) };
    result
}

pub(super) struct TextMark {
    origin: Point,
    layout: TextLayout,
}

struct TextLayout {
    units: Vec<u16>,
    lines: Vec<TextLine>,
    width: i32,
    content_width: i32,
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
    fn new(metrics: TextMetrics<'_>, mut units: Vec<u16>, limit: i32) -> Self {
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
        for grapheme in text.graphemes(true) {
            let end = offset + grapheme.encode_utf16().count();
            if matches!(grapheme, "\n" | "\r" | "\r\n") {
                lines.push(line);
                line = TextLine {
                    stops: vec![(end, 0)],
                };
            } else {
                let mut width = (metrics.measure)(&units[line.start()..end]);
                if width > limit && offset > line.start() {
                    lines.push(line);
                    line = TextLine {
                        stops: vec![(offset, 0)],
                    };
                    width = (metrics.measure)(&units[offset..end]);
                }
                line.stops.push((end, width));
            }
            offset = end;
        }
        lines.push(line);
        let content_width = lines.iter().map(|line| line.end().1).max().unwrap_or(0);
        let width = content_width.clamp(metrics.chinese_width.min(limit), limit);
        Self {
            units,
            lines,
            width,
            content_width,
            line_height: metrics.line_height,
        }
    }

    fn caret(&self, index: usize) -> Point {
        let row = self
            .lines
            .partition_point(|line| line.start() <= index)
            .saturating_sub(1);
        Point {
            x: self.lines[row].x_at(index).min(self.width),
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
    #[cfg(test)]
    pub fn new(dc: HDC, font: &Font, crop: Area, anchor: Point, text: Vec<u16>) -> Self {
        with_metrics(dc, font, |metrics| {
            Self::layout(metrics, crop, anchor, text)
        })
    }

    fn layout(metrics: TextMetrics<'_>, crop: Area, anchor: Point, text: Vec<u16>) -> Self {
        let x = anchor.x.min((crop.width() - metrics.chinese_width).max(0));
        let layout = TextLayout::new(metrics, text, crop.width() - x);
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

    fn fits(&self, crop: Area) -> bool {
        self.layout.content_width <= self.layout.width
            && self.layout.height() <= crop.height() - self.origin.y
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

struct Composition {
    units: Vec<u16>,
    caret: usize,
}

pub(super) struct TextDraft {
    crop: Area,
    anchor: Point,
    draft: TextMark,
    frame: Area,
    selection: Range<usize>,
    composition: Option<Range<usize>>,
    caret: usize,
    caret_visible: bool,
}

impl TextDraft {
    fn new(metrics: TextMetrics<'_>, crop: Area, anchor: Point) -> Self {
        let (draft, frame) = Self::layout(metrics, crop, anchor, Vec::new());
        Self {
            crop,
            anchor,
            draft,
            frame,
            selection: 0..0,
            composition: None,
            caret: 0,
            caret_visible: true,
        }
    }

    fn layout(
        metrics: TextMetrics<'_>,
        crop: Area,
        anchor: Point,
        units: Vec<u16>,
    ) -> (TextMark, Area) {
        let mut caret_space = CARET_WIDTH.min((crop.width() - metrics.chinese_width).max(0));
        let text_width = crop.width() - caret_space;
        let mut horizontal = INPUT_PADDING.min((text_width - metrics.chinese_width).max(0) / 2);
        let content = Area {
            right: text_width - horizontal,
            ..crop.local_bounds()
        };
        let mut draft = TextMark::layout(
            metrics,
            content,
            anchor
                + Point {
                    x: horizontal,
                    y: 0,
                },
            units,
        );
        if horizontal + caret_space > 0 && !draft.fits(crop) {
            // Lay out the text first, then restore as much spacing as fits around it.
            draft = TextMark::layout(metrics, crop, anchor, draft.layout.units);
            let available = crop.width() - draft.bounds().right;
            caret_space = caret_space.min(available);
            horizontal = horizontal.min((available - caret_space) / 2);
            draft.origin.x += horizontal;
        }
        let extra_height = (crop.height() - draft.layout.height()).max(0);
        let vertical = INPUT_PADDING.min(extra_height / 2);
        draft.origin.y = (anchor.y + vertical).clamp(vertical, extra_height - vertical);
        let text = draft.bounds();
        let frame = Area {
            left: text.left - horizontal,
            top: text.top - vertical,
            right: text.right + caret_space + horizontal,
            bottom: text.bottom + vertical,
        };
        (draft, frame)
    }

    fn update(
        &mut self,
        metrics: TextMetrics<'_>,
        mut units: Vec<u16>,
        selection: Range<usize>,
        composition: Option<Composition>,
    ) {
        self.selection = selection.start.min(units.len())..selection.end.min(units.len());
        self.caret = self.selection.end;
        self.composition = composition.map(|composition| {
            let start = self.selection.start;
            let length = composition.units.len();
            self.caret = start + composition.caret.min(length);
            units.splice(self.selection.clone(), composition.units);
            start..start + length
        });
        (self.draft, self.frame) = Self::layout(metrics, self.crop, self.anchor, units);
        self.caret_visible = true;
    }

    fn commit(
        &self,
        metrics: TextMetrics<'_>,
        units: Vec<u16>,
    ) -> Result<Option<TextMark>, Overflow> {
        if !has_text(&units) {
            return Ok(None);
        }
        let (mark, _) = Self::layout(metrics, self.crop, self.anchor, units);
        if mark.fits(self.crop) {
            Ok(Some(mark))
        } else {
            Err(Overflow)
        }
    }

    fn index_at(&self, point: Point) -> Option<usize> {
        self.composition
            .is_none()
            .then(|| self.draft.layout.index_at(point - self.draft.origin))
    }

    pub fn bounds(&self) -> Area {
        self.frame
    }

    pub fn overflows(&self) -> bool {
        !self.draft.fits(self.crop) && has_text(&self.draft.layout.units)
    }

    fn caret_position(&self, index: usize) -> Point {
        let crop = self.crop;
        let offset = self.draft.layout.caret(index);
        let last_column = crop.width() - CARET_WIDTH;
        let inset = i32::from(last_column > 1);
        Point {
            x: (self.draft.origin.x + offset.x).clamp(inset, last_column - inset),
            y: (self.draft.origin.y + offset.y)
                .min((crop.height() - self.draft.layout.line_height).max(0)),
        }
    }

    pub(super) fn caret_bounds(&self) -> Area {
        let point = self.caret_position(self.caret);
        let height = self.draft.layout.line_height.min(self.crop.height());
        let gap = 2.min((height - 1) / 2);
        Area {
            left: point.x,
            top: point.y + gap,
            right: point.x + CARET_WIDTH,
            bottom: point.y + height - gap,
        }
    }

    pub fn visible_caret(&self) -> Option<Area> {
        self.caret_visible.then(|| self.caret_bounds())
    }

    pub unsafe fn paint(&self, dc: HDC, font: &Font, color: COLORREF) {
        let layout = &self.draft.layout;
        let origin = self.draft.origin;
        let edge = if self.overflows() {
            OVERFLOW
        } else {
            appearance::ACCENT
        };
        let brush = CreateSolidBrush(edge);
        if !brush.is_null() {
            FrameRect(dc, &self.frame.rect(), brush);
            DeleteObject(brush);
        }
        let state = SaveDC(dc);
        if state != 0 {
            let text = self.draft.bounds();
            IntersectClipRect(dc, text.left, text.top, text.right, text.bottom);
            self.draft.paint(dc, font, origin, color);
            RestoreDC(dc, state);
        }
        let underlined = self.composition.as_ref().unwrap_or(&self.selection);
        if !underlined.is_empty() {
            for (row, line) in layout.lines.iter().enumerate() {
                let start = underlined.start.max(line.start());
                let end = underlined.end.min(line.end().0);
                if start < end {
                    let y = origin.y + (row as i32 + 1) * layout.line_height - 2;
                    appearance::fill(
                        dc,
                        &RECT {
                            left: origin.x + line.x_at(start),
                            top: y,
                            right: origin.x + line.x_at(end).min(layout.width),
                            bottom: y + 1,
                        },
                        appearance::ACCENT,
                    );
                }
            }
        }
        if let Some(caret) = self.visible_caret() {
            appearance::fill(dc, &caret.rect(), SELECTION_BORDER);
        }
    }
}

fn has_text(units: &[u16]) -> bool {
    char::decode_utf16(units.iter().copied()).any(|character| {
        !character
            .unwrap_or(char::REPLACEMENT_CHARACTER)
            .is_whitespace()
    })
}

#[derive(Debug)]
pub(super) struct Overflow;

struct TextInput {
    native: NativeEdit,
    draft: TextDraft,
}

#[derive(Default)]
pub(super) struct TextEditor {
    input: Option<TextInput>,
    font: Option<Font>,
    revision: u64,
}

impl TextEditor {
    #[cfg(test)]
    pub fn with_font(font: Font) -> Self {
        Self {
            font: Some(font),
            ..Self::default()
        }
    }

    pub fn font(&self) -> Option<&Font> {
        self.font.as_ref()
    }

    pub fn is_open(&self) -> bool {
        self.input.is_some()
    }

    pub fn open(&mut self, overlay: HWND, dc: HDC, crop: Area, anchor: Point) {
        if self.font.is_none() {
            self.font = Font::new(dc).ok();
        }
        let Some(font) = self.font.as_ref() else {
            return;
        };
        let Some(native) = NativeEdit::new(overlay, font) else {
            return;
        };
        let draft = with_metrics(dc, font, |metrics| TextDraft::new(metrics, crop, anchor));
        let source = native.hwnd;
        self.input
            .insert(TextInput { native, draft })
            .native
            .focus();
        self.refresh(dc, source);
    }

    pub fn overlay_for(&self, source: HWND) -> Option<HWND> {
        self.input
            .as_ref()
            .filter(|input| input.native.hwnd == source)
            .map(|input| input.native.overlay)
    }

    pub fn refresh(&mut self, dc: HDC, source: HWND) {
        let Some(input) = self
            .input
            .as_mut()
            .filter(|input| input.native.hwnd == source)
        else {
            return;
        };
        let font = self.font.as_ref().unwrap();
        let (units, selection, composition) = input.native.read();
        with_metrics(dc, font, |metrics| {
            input.draft.update(metrics, units, selection, composition)
        });
        let draft = &input.draft;
        let position = draft.crop.origin() + draft.caret_position(draft.caret);
        input.native.place_candidate(position, font.line_height);
        input.native.restart_blink();
        self.revision += 1;
    }

    pub fn click(&self, point: Point) -> bool {
        let Some(input) = self
            .input
            .as_ref()
            .filter(|input| input.draft.bounds().contains(point))
        else {
            return false;
        };
        if let Some(index) = input.draft.index_at(point) {
            input.native.set_caret(index);
        }
        true
    }

    pub fn commit(&mut self, dc: HDC) -> Result<Option<TextMark>, Overflow> {
        let Some(input) = self.input.as_ref() else {
            return Ok(None);
        };
        let units = input.native.finish_composition();
        let result = with_metrics(dc, self.font.as_ref().unwrap(), |metrics| {
            input.draft.commit(metrics, units)
        });
        if result.is_err() {
            input.native.focus();
            self.refresh(dc, input.native.hwnd);
        } else {
            self.cancel();
        }
        result
    }

    pub fn cancel(&mut self) {
        if let Some(input) = self.input.take() {
            input.native.close();
        }
    }

    pub fn stop_blink(&self) {
        if let Some(input) = &self.input {
            input.native.stop_blink();
        }
    }

    pub fn shutdown(&mut self) {
        self.input = None;
    }

    pub fn blink(&mut self) {
        if let Some(input) = &mut self.input {
            input.draft.caret_visible = !input.draft.caret_visible;
        }
    }

    pub fn preview(&self) -> Option<(u64, &TextDraft)> {
        Some((self.revision, &self.input.as_ref()?.draft))
    }

    pub fn ime_position(&self, source: HWND, desktop: Area, info: &mut IMECHARPOSITION) -> bool {
        let Some(input) = self
            .input
            .as_ref()
            .filter(|input| input.native.hwnd == source)
        else {
            return false;
        };
        let draft = &input.draft;
        let index = draft.composition.as_ref().map_or(draft.caret, |range| {
            range.start + (info.dwCharPos as usize).min(range.len())
        });
        native::report_position(
            draft.caret_position(index),
            draft.draft.layout.line_height,
            draft.crop,
            desktop,
            info,
        );
        true
    }

    #[cfg(test)]
    pub fn hwnd(&self) -> HWND {
        self.input.as_ref().unwrap().native.hwnd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> TextMetrics<'static> {
        TextMetrics {
            chinese_width: 8,
            line_height: 16,
            measure: &|units| String::from_utf16_lossy(units).graphemes(true).count() as i32 * 8,
        }
    }

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn composition_replaces_the_utf16_selection_in_the_preview() {
        let metrics = metrics();
        let crop = Area {
            left: 30,
            top: 40,
            right: 110,
            bottom: 90,
        };
        let mut draft = TextDraft::new(metrics, crop, Point { x: 0, y: 0 });
        let value = "A🧪B";
        draft.update(
            metrics,
            units(value),
            1..3,
            Some(Composition {
                units: units("中文"),
                caret: 1,
            }),
        );
        assert_eq!(draft.draft.layout.units, units("A中文B"));
        assert_eq!(draft.composition, Some(1..3));
        assert_eq!(
            draft.caret_position(draft.caret).x,
            draft.draft.origin.x + 16
        );
        assert_eq!(draft.index_at(draft.draft.origin), None);

        draft.update(metrics, units(value), 1..3, None);
        let after_emoji = draft.draft.origin + Point { x: 16, y: 0 };
        assert_eq!(draft.index_at(after_emoji), Some(3));
        assert_eq!(
            draft
                .commit(metrics, units(value))
                .unwrap()
                .unwrap()
                .layout
                .units,
            units(value)
        );
    }

    #[test]
    fn draft_recomputes_fit_after_overflow_and_respects_crlf() {
        let metrics = metrics();
        let crop = Area {
            left: 0,
            top: 0,
            right: 16,
            bottom: 16,
        };
        let mut draft = TextDraft::new(metrics, crop, crop.origin());
        draft.update(metrics, units("中文字"), 3..3, None);
        assert!(draft.overflows());
        assert!(draft.commit(metrics, units("中文字")).is_err());
        assert!(draft.commit(metrics, units("\r\n ")).unwrap().is_none());

        draft.update(metrics, units("中文"), 2..2, None);
        assert!(!draft.overflows());
        assert_eq!(
            draft
                .commit(metrics, units("中文"))
                .unwrap()
                .unwrap()
                .bounds(),
            crop
        );

        let crop = Area {
            right: 40,
            bottom: 48,
            ..crop
        };
        let mut draft = TextDraft::new(metrics, crop, crop.origin());
        draft.update(metrics, units("e\u{301}\r\n🧪x"), 7..7, None);
        let end_of_emoji = draft.draft.origin + Point { x: 8, y: 16 };
        assert_eq!(draft.index_at(end_of_emoji), Some(6));
        assert_eq!(draft.draft.bounds().height(), 32);
    }
}
