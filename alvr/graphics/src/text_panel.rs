//! Text and simple shapes drawn on the CPU into an RGBA8 buffer: the lobby's HUD message
//! (`text_coverage`) and the tuning menu (`render_menu`), which the client shows as an OpenXR quad
//! layer. One font, Ubuntu Medium, as the lobby always had.
//!
//! `Canvas` and `render_menu` give PREMULTIPLIED alpha: each colour channel is already multiplied by
//! the alpha of its pixel (a pixel half covered by white is 128, 128, 128, 128). The quad layer is
//! submitted without `XR_COMPOSITION_LAYER_UNPREMULTIPLIED_ALPHA_BIT` and with
//! `XR_COMPOSITION_LAYER_BLEND_TEXTURE_SOURCE_ALPHA_BIT`, so that the clear parts show what is behind.
//! Row 0 of the buffer is the top of the image, as the texture is uploaded.

use crate::tuning::{
    MENU_FOOTER_TOP, MENU_ROWS_TOP, MENU_TAB_HEIGHT, MenuContent, ROW_BAR_LEFT, ROW_BAR_RIGHT,
    ROW_LABEL_LEFT, ROW_MARK_LEFT, ROW_VALUE_RIGHT, menu_row_height,
};
use glyph_brush_layout::{
    BuiltInLineBreaker, FontId, GlyphPositioner, HorizontalAlign, Layout, SectionGeometry,
    SectionText, VerticalAlign,
    ab_glyph::{Font, FontRef, ScaleFont},
};

/// The font of the lobby and of the menu
pub fn ui_font() -> FontRef<'static> {
    FontRef::try_from_slice(include_bytes!("../resources/Ubuntu-Medium.ttf")).unwrap()
}

/// Lays out `text` with `layout` at `geometry` and calls `pixel(x, y, coverage)` for every pixel the
/// glyphs touch (coverage 0..1). `x`, `y` are not clipped: they may be negative or beyond the buffer
pub fn text_coverage(
    text: &str,
    scale: f32,
    layout: Layout<BuiltInLineBreaker>,
    geometry: &SectionGeometry,
    mut pixel: impl FnMut(i32, i32, f32),
) {
    let font = ui_font();
    let section_glyphs = layout.calculate_glyphs(
        &[&font],
        geometry,
        &[SectionText {
            text,
            scale: scale.into(),
            font_id: FontId(0),
        }],
    );
    let scaled_font = font.as_scaled(scale);

    for section_glyph in section_glyphs {
        if let Some(outlined) = scaled_font.outline_glyph(section_glyph.glyph) {
            let bounds = outlined.px_bounds();

            outlined.draw(|x, y, alpha| {
                pixel(
                    x as i32 + bounds.min.x as i32,
                    y as i32 + bounds.min.y as i32,
                    alpha,
                );
            });
        }
    }
}

/// A colour with straight (not premultiplied) alpha, 0..1 per channel
pub type Rgba = [f32; 4];

/// An RGBA8 image with premultiplied alpha (see the module), row 0 at the top
#[derive(Clone, Debug, PartialEq)]
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl Canvas {
    /// Fully transparent
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height * 4],
        }
    }

    /// `color` over the pixel with `coverage` (0..1), source-over in premultiplied alpha. Pixels off the
    /// image are left out
    pub fn blend(&mut self, x: i32, y: i32, color: Rgba, coverage: f32) {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return;
        }
        let alpha = (color[3] * coverage).clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }
        let at = (y as usize * self.width + x as usize) * 4;
        let source = [color[0] * alpha, color[1] * alpha, color[2] * alpha, alpha];
        for (channel, source) in self.pixels[at..at + 4].iter_mut().zip(source) {
            let value = source + f32::from(*channel) / 255.0 * (1.0 - alpha);
            *channel = (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }

    /// A filled rectangle, pixel edges (x0..x1, y0..y1, end excluded), clipped to the image
    pub fn fill_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: Rgba) {
        let clip_x = |x: f32| x.round().clamp(0.0, self.width as f32) as i32;
        let clip_y = |y: f32| y.round().clamp(0.0, self.height as f32) as i32;
        let (x0, x1, y0, y1) = (clip_x(x0), clip_x(x1), clip_y(y0), clip_y(y1));
        for y in y0..y1 {
            for x in x0..x1 {
                self.blend(x, y, color, 1.0);
            }
        }
    }

    /// A filled circle with a soft edge of one pixel
    pub fn fill_circle(&mut self, cx: f32, cy: f32, radius: f32, color: Rgba) {
        let reach = radius.ceil() as i32 + 1;
        for y in (cy as i32 - reach)..=(cy as i32 + reach) {
            for x in (cx as i32 - reach)..=(cx as i32 + reach) {
                let distance = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
                let coverage = (radius - distance + 0.5).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    self.blend(x, y, color, coverage);
                }
            }
        }
    }

    /// One line of text at `(x, y)` (pixels) aligned by `h_align` / `v_align`, `size` in pixels
    pub fn draw_text(
        &mut self,
        text: &str,
        (x, y): (f32, f32),
        size: f32,
        (h_align, v_align): (HorizontalAlign, VerticalAlign),
        color: Rgba,
    ) {
        text_coverage(
            text,
            size,
            Layout::default_single_line()
                .h_align(h_align)
                .v_align(v_align),
            &SectionGeometry {
                screen_position: (x, y),
                ..Default::default()
            },
            |px, py, coverage| self.blend(px, py, color, coverage),
        );
    }
}

const MENU_BACKGROUND: Rgba = [0.06, 0.07, 0.09, 0.82];
const MENU_TAB: Rgba = [0.18, 0.20, 0.24, 1.0];
const MENU_TAB_ACTIVE: Rgba = [0.15, 0.45, 0.60, 1.0];
const MENU_HOVER: Rgba = [1.0, 1.0, 1.0, 0.12];
const MENU_SELECTED: Rgba = [0.2, 0.9, 1.0, 0.22];
const MENU_TEXT: Rgba = [1.0, 1.0, 1.0, 1.0];
const MENU_DIM_TEXT: Rgba = [0.75, 0.78, 0.82, 1.0];
const MENU_BAR_TRACK: Rgba = [1.0, 1.0, 1.0, 0.18];
const MENU_BAR_FILL: Rgba = [0.2, 0.9, 1.0, 0.9];
const MENU_OVERRIDE_MARK: Rgba = [1.0, 0.72, 0.15, 1.0];
const MENU_CURSOR: Rgba = [1.0, 1.0, 1.0, 0.95];
const MENU_CURSOR_RING: Rgba = [0.0, 0.0, 0.0, 0.8];

/// The tuning menu as an RGBA8 image of `width` x `height`, PREMULTIPLIED alpha (see the module), row 0
/// at the top. Layout from the `MENU_*` / `ROW_*` constants of `tuning` (fractions of the panel), the
/// same ones its hit test uses: the tab strip, the rows (label, value, bar, override mark), the stats
/// footer, the cursors of the hands
pub fn render_menu(content: &MenuContent, width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as f32, height as f32);
    let mut canvas = Canvas::new(width as usize, height as usize);
    canvas.fill_rect(0.0, 0.0, w, h, MENU_BACKGROUND);

    // Tabs
    let tab_count = content.tabs.len().max(1) as f32;
    let tab_bottom = MENU_TAB_HEIGHT * h;
    let text_size = (h * 0.045).max(8.0);
    for (index, tab) in content.tabs.iter().enumerate() {
        let x0 = index as f32 / tab_count * w;
        let x1 = (index + 1) as f32 / tab_count * w;
        let color = if tab.active {
            MENU_TAB_ACTIVE
        } else {
            MENU_TAB
        };
        canvas.fill_rect(x0 + 2.0, 2.0, x1 - 2.0, tab_bottom - 2.0, color);
        if tab.hovered {
            canvas.fill_rect(x0 + 2.0, 2.0, x1 - 2.0, tab_bottom - 2.0, MENU_HOVER);
        }
        canvas.draw_text(
            &tab.label,
            ((x0 + x1) / 2.0, tab_bottom / 2.0),
            text_size * 0.85,
            (HorizontalAlign::Center, VerticalAlign::Center),
            MENU_TEXT,
        );
    }

    // Rows
    let row_height = menu_row_height() * h;
    for (index, row) in content.rows.iter().enumerate() {
        let y0 = MENU_ROWS_TOP * h + index as f32 * row_height;
        let y1 = y0 + row_height;
        let middle = (y0 + y1) / 2.0;
        if row.selected {
            canvas.fill_rect(0.0, y0 + 1.0, w, y1 - 1.0, MENU_SELECTED);
        }
        if row.hovered {
            canvas.fill_rect(0.0, y0 + 1.0, w, y1 - 1.0, MENU_HOVER);
        }
        if row.overridden {
            canvas.fill_circle(
                ROW_MARK_LEFT * w + row_height * 0.12,
                middle,
                row_height * 0.1,
                MENU_OVERRIDE_MARK,
            );
        }
        canvas.draw_text(
            &row.label,
            (ROW_LABEL_LEFT * w, middle),
            text_size,
            (HorizontalAlign::Left, VerticalAlign::Center),
            MENU_TEXT,
        );
        canvas.draw_text(
            &row.value,
            (ROW_VALUE_RIGHT * w, middle),
            text_size,
            (HorizontalAlign::Right, VerticalAlign::Center),
            MENU_TEXT,
        );
        if let Some(bar) = row.bar {
            let (x0, x1) = (ROW_BAR_LEFT * w, ROW_BAR_RIGHT * w);
            let half = (row_height * 0.12).max(2.0);
            canvas.fill_rect(x0, middle - half, x1, middle + half, MENU_BAR_TRACK);
            let filled = x0 + (x1 - x0) * bar.clamp(0.0, 1.0);
            canvas.fill_rect(x0, middle - half, filled, middle + half, MENU_BAR_FILL);
        }
    }

    // Stats
    let footer_top = MENU_FOOTER_TOP * h;
    canvas.fill_rect(0.0, footer_top, w, footer_top + 2.0, MENU_BAR_TRACK);
    canvas.draw_text(
        &content.footer,
        (ROW_LABEL_LEFT * w, (footer_top + h) / 2.0),
        text_size * 0.85,
        (HorizontalAlign::Left, VerticalAlign::Center),
        MENU_DIM_TEXT,
    );

    // Cursors
    let radius = (h * 0.012).max(2.0);
    for cursor in &content.cursors {
        let (x, y) = (cursor.x * w, cursor.y * h);
        canvas.fill_circle(x, y, radius + 2.0, MENU_CURSOR_RING);
        canvas.fill_circle(x, y, radius, MENU_CURSOR);
    }

    canvas.pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tuning::{MenuRow, MenuTab};
    use alvr_common::glam::Vec2;

    #[test]
    fn canvas_blends_premultiplied() {
        let mut canvas = Canvas::new(4, 2);
        canvas.fill_rect(0.0, 0.0, 2.0, 2.0, [1.0, 1.0, 1.0, 0.5]);
        assert_eq!(&canvas.pixels[0..4], &[128, 128, 128, 128]);
        // Opaque red over it
        canvas.blend(0, 0, [1.0, 0.0, 0.0, 1.0], 1.0);
        assert_eq!(&canvas.pixels[0..4], &[255, 0, 0, 255]);
        // Untouched pixel, off the image
        assert_eq!(&canvas.pixels[8..12], &[0, 0, 0, 0]);
        canvas.blend(-1, 0, MENU_TEXT, 1.0);
        canvas.blend(4, 0, MENU_TEXT, 1.0);
        // No channel above the alpha anywhere: premultiplied
        for pixel in canvas.pixels.chunks(4) {
            assert!(pixel[..3].iter().all(|&c| c <= pixel[3]));
        }
    }

    #[test]
    fn text_draws_pixels_where_placed() {
        let mut canvas = Canvas::new(200, 60);
        canvas.draw_text(
            "Hello",
            (10.0, 30.0),
            30.0,
            (HorizontalAlign::Left, VerticalAlign::Center),
            MENU_TEXT,
        );
        let lit = canvas
            .pixels
            .chunks(4)
            .enumerate()
            .filter(|(_, pixel)| pixel[3] > 0)
            .map(|(index, _)| index % 200)
            .collect::<Vec<_>>();
        assert!(lit.len() > 100);
        assert!(lit.iter().all(|&x| (8..120).contains(&x)));
    }

    #[test]
    fn menu_buffer_has_the_size_and_content() {
        let content = MenuContent {
            tabs: vec![
                MenuTab {
                    label: "Hands".into(),
                    active: true,
                    hovered: false,
                },
                MenuTab {
                    label: "Arms".into(),
                    active: false,
                    hovered: true,
                },
            ],
            rows: vec![MenuRow {
                label: "Skin preference".into(),
                value: "150 %".into(),
                bar: Some(0.15),
                overridden: true,
                hovered: true,
                selected: false,
            }],
            footer: "skin 90 %".into(),
            cursors: vec![Vec2::new(0.5, 0.5)],
        };
        let buffer = render_menu(&content, 256, 192);
        assert_eq!(buffer.len(), 256 * 192 * 4);
        // Not all one colour, opaque somewhere (text, tabs), premultiplied everywhere
        assert!(buffer.chunks(4).any(|pixel| pixel != &buffer[0..4]));
        assert!(buffer.chunks(4).any(|pixel| pixel[3] == 255));
        for pixel in buffer.chunks(4) {
            assert!(pixel[..3].iter().all(|&c| c <= pixel[3]), "{pixel:?}");
        }
        // The override mark is amber
        assert!(
            buffer
                .chunks(4)
                .any(|pixel| pixel[0] > 200 && pixel[1] > 120 && pixel[2] < 80)
        );
    }
}
