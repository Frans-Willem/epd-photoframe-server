//! `Drawable` — the unit of paint that the layout-driven overlay
//! renderer composes. Each variant carries everything it needs to
//! both *measure* itself (used by taffy's layout pass) and *draw*
//! itself onto a `Pixmap` at an absolute (x, y) computed by taffy.
//!
//! Adding a new visual primitive is one new variant + two arms in
//! `Drawable::measure` and `Drawable::draw`. Callers (overlay impls)
//! build a small `taffy::TaffyTree<Drawable>`, attach `Drawable`
//! context to nodes that should paint, and let [`walk`] do the rest.

use std::sync::LazyLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use taffy::{NodeId, Size, TaffyTree, TraversePartialTree};
use tiny_skia::Pixmap;

use crate::config::ColorConfig;
use crate::draw::{draw_line, line_width, paint_rounded_rect};

static TEXT_FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../../assets/LiberationSans-Bold.ttf"))
        .expect("bundled text font is invalid")
});

static ICON_FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../../assets/WeatherIcons-Regular.ttf"))
        .expect("bundled icon font is invalid")
});

#[allow(dead_code)] // `Icon` is used by the Stage 4 multi-day cells.
pub enum Drawable {
    /// Liberation Sans Bold text.
    Text {
        content: String,
        size: f32,
        color: ColorConfig,
    },
    /// Single Weather Icons glyph.
    Icon {
        glyph: char,
        size: f32,
        color: ColorConfig,
    },
    /// Rounded-rect fill — typically attached to a parent node so it
    /// paints behind the children. Sized by the parent box, not by
    /// the variant itself (`measure` returns ZERO).
    Background { color: ColorConfig, radius: f32 },
    /// Icon followed by text on a *shared baseline*. Used where flex
    /// baseline alignment is awkward across fonts with different
    /// metrics (the today-weather line: large icon next to smaller
    /// temperature label, both reading naturally on the same line).
    IconText {
        icon: char,
        icon_size: f32,
        gap: f32,
        text: String,
        text_size: f32,
        color: ColorConfig,
    },
}

impl Drawable {
    /// Intrinsic size — used by taffy's measure callback for leaves.
    /// `Background` returns ZERO since its size is whatever its
    /// parent node decides.
    pub fn measure(&self) -> Size<f32> {
        match self {
            Drawable::Text { content, size, .. } => {
                let font: &FontRef<'static> = &TEXT_FONT;
                let scale = PxScale::from(*size);
                Size {
                    width: line_width(font, scale, content),
                    height: font.as_scaled(scale).height(),
                }
            }
            Drawable::Icon { glyph, size, .. } => {
                let font: &FontRef<'static> = &ICON_FONT;
                let scale = PxScale::from(*size);
                let s = glyph.to_string();
                Size {
                    width: line_width(font, scale, &s),
                    height: font.as_scaled(scale).height(),
                }
            }
            Drawable::Background { .. } => Size::ZERO,
            Drawable::IconText {
                icon,
                icon_size,
                gap,
                text,
                text_size,
                ..
            } => {
                let icon_font: &FontRef<'static> = &ICON_FONT;
                let icon_scale = PxScale::from(*icon_size);
                let text_font: &FontRef<'static> = &TEXT_FONT;
                let text_scale = PxScale::from(*text_size);
                let icon_str = icon.to_string();
                Size {
                    width: line_width(icon_font, icon_scale, &icon_str)
                        + gap
                        + line_width(text_font, text_scale, text),
                    height: icon_font
                        .as_scaled(icon_scale)
                        .height()
                        .max(text_font.as_scaled(text_scale).height()),
                }
            }
        }
    }

    /// Paint at the absolute `(x, y)` computed by taffy. `w` / `h`
    /// are the node's computed box size — only `Background` uses
    /// them (text/icon glyphs draw at their measured size).
    pub fn draw(&self, canvas: &mut Pixmap, x: f32, y: f32, w: f32, h: f32) {
        match self {
            Drawable::Text {
                content,
                size,
                color,
            } => {
                let font: &FontRef<'static> = &TEXT_FONT;
                let scale = PxScale::from(*size);
                let baseline = y + font.as_scaled(scale).ascent();
                draw_line(
                    canvas,
                    font,
                    scale,
                    x,
                    baseline,
                    content,
                    color.to_tiny_skia(),
                    None,
                );
            }
            Drawable::Icon { glyph, size, color } => {
                let font: &FontRef<'static> = &ICON_FONT;
                let scale = PxScale::from(*size);
                let baseline = y + font.as_scaled(scale).ascent();
                let s = glyph.to_string();
                draw_line(
                    canvas,
                    font,
                    scale,
                    x,
                    baseline,
                    &s,
                    color.to_tiny_skia(),
                    None,
                );
            }
            Drawable::Background { color, radius } => {
                paint_rounded_rect(canvas, x, y, w, h, *radius, *color);
            }
            Drawable::IconText {
                icon,
                icon_size,
                gap,
                text,
                text_size,
                color,
            } => {
                let icon_font: &FontRef<'static> = &ICON_FONT;
                let icon_scale = PxScale::from(*icon_size);
                let text_font: &FontRef<'static> = &TEXT_FONT;
                let text_scale = PxScale::from(*text_size);
                let icon_ascent = icon_font.as_scaled(icon_scale).ascent();
                let text_ascent = text_font.as_scaled(text_scale).ascent();
                let baseline = y + icon_ascent.max(text_ascent);
                let icon_str = icon.to_string();
                let icon_w = line_width(icon_font, icon_scale, &icon_str);
                draw_line(
                    canvas,
                    icon_font,
                    icon_scale,
                    x,
                    baseline,
                    &icon_str,
                    color.to_tiny_skia(),
                    None,
                );
                draw_line(
                    canvas,
                    text_font,
                    text_scale,
                    x + icon_w + gap,
                    baseline,
                    text,
                    color.to_tiny_skia(),
                    None,
                );
            }
        }
    }
}

/// Depth-first walk over a laid-out taffy tree. Visits each node
/// that has a `Drawable` context, passing the node's absolute
/// position and computed size to `visit`. **Parent before children**
/// so a `Background` on a parent paints first and its children draw
/// on top.
///
/// `(ox, oy)` is the origin offset to add to taffy's
/// layout-relative positions — typically the box's anchor point on
/// the larger canvas.
pub fn walk(
    tree: &TaffyTree<Drawable>,
    node: NodeId,
    ox: f32,
    oy: f32,
    visit: &mut impl FnMut(f32, f32, f32, f32, &Drawable),
) {
    let layout = tree.layout(node).expect("layout was computed");
    let x = ox + layout.location.x;
    let y = oy + layout.location.y;
    if let Some(ctx) = tree.get_node_context(node) {
        visit(x, y, layout.size.width, layout.size.height, ctx);
    }
    let children: Vec<_> = tree.child_ids(node).collect();
    for child in children {
        walk(tree, child, x, y, visit);
    }
}
