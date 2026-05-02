//! [`Drawable`] is the trait that the layout-driven overlay renderer
//! composes. Implementors know how to *measure* themselves (used by
//! taffy's layout pass for intrinsic-sized leaves) and *draw*
//! themselves onto a `Pixmap` at an absolute `(x, y)` computed by
//! taffy.
//!
//! Two implementors today: [`GenericDrawable`] — a small grab-bag of
//! rendering primitives (`MultiText` for one or more shared-baseline
//! text spans, `RoundedRect` for a flat fill) — and
//! `overlays::battery_indicator::BatteryNode`, which paints a single
//! self-contained battery icon.
//!
//! Each overlay builds a `TaffyTree<MyDrawable>`, attaches its
//! drawable context to the nodes that should paint, and calls
//! [`paint`] — which runs the layout pass and then walks the tree
//! (parent before children, so backgrounds end up underneath).

use std::sync::LazyLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use taffy::prelude::*;
use tiny_skia::Pixmap;

use crate::config::{ColorConfig, Position};
use crate::draw::{draw_line, line_width, paint_rounded_rect};

/// Anything that can be sized and painted into a taffy node.
///
/// `measure` defaults to `ZERO` for the common case of leaves that
/// declare an explicit `size` in their `Style` — taffy never calls
/// the measure callback for those nodes, so the default is fine.
/// Override it for content-sized leaves (text, glyphs).
pub trait Drawable {
    fn measure(&self) -> Size<f32> {
        Size::ZERO
    }
    fn draw(&self, canvas: &mut Pixmap, x: f32, y: f32, w: f32, h: f32);
}

/// Map a `Position` (one of the 8 corners + edges) to the flex
/// `justify_content` (horizontal) / `align_items` (vertical) pair
/// that anchors a single child in that position inside a viewport-
/// sized container with uniform padding.
pub fn position_to_flex(p: Position) -> (JustifyContent, AlignItems) {
    use Position::*;
    match p {
        TopLeft => (JustifyContent::Start, AlignItems::Start),
        Top => (JustifyContent::Center, AlignItems::Start),
        TopRight => (JustifyContent::End, AlignItems::Start),
        Left => (JustifyContent::Start, AlignItems::Center),
        Right => (JustifyContent::End, AlignItems::Center),
        BottomLeft => (JustifyContent::Start, AlignItems::End),
        Bottom => (JustifyContent::Center, AlignItems::End),
        BottomRight => (JustifyContent::End, AlignItems::End),
    }
}

/// Compute layout for `tree` rooted at `root` (sized to the canvas)
/// and paint every node with a [`Drawable`] context. Visits parent
/// before children so a `RoundedRect` on a parent paints underneath
/// its children's content.
pub fn paint<C: Drawable>(tree: &mut TaffyTree<C>, root: NodeId, canvas: &mut Pixmap) {
    let (w, h) = (canvas.width() as f32, canvas.height() as f32);
    tree.compute_layout_with_measure(
        root,
        Size {
            width: AvailableSpace::Definite(w),
            height: AvailableSpace::Definite(h),
        },
        |_known, _avail, _id, ctx, _style| ctx.map(|d: &mut C| d.measure()).unwrap_or(Size::ZERO),
    )
    .expect("compute layout");
    paint_visitor(tree, root, 0.0, 0.0, canvas);
}

fn paint_visitor<C: Drawable>(
    tree: &TaffyTree<C>,
    node: NodeId,
    ox: f32,
    oy: f32,
    canvas: &mut Pixmap,
) {
    let layout = tree.layout(node).expect("layout was computed");
    let x = ox + layout.location.x;
    let y = oy + layout.location.y;
    if let Some(ctx) = tree.get_node_context(node) {
        ctx.draw(canvas, x, y, layout.size.width, layout.size.height);
    }
    for child_id in tree.child_ids(node) {
        paint_visitor(tree, child_id, x, y, canvas);
    }
}

static TEXT_FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../../assets/LiberationSans-Bold.ttf"))
        .expect("bundled text font is invalid")
});

static ICON_FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../../assets/WeatherIcons-Regular.ttf"))
        .expect("bundled icon font is invalid")
});

/// Liberation Sans Bold — the body text font.
pub fn text_font() -> &'static FontRef<'static> {
    &TEXT_FONT
}

/// Weather Icons Regular — single-glyph weather pictograms.
pub fn icon_font() -> &'static FontRef<'static> {
    &ICON_FONT
}

/// One contiguous run of text in a single font / size / color. Used
/// as the building block of [`GenericDrawable::MultiText`]: each span
/// is drawn flush against the previous one, with `gap_before` extra
/// horizontal space inserted *between* spans (ignored on the first).
pub struct TextSpan {
    pub content: String,
    pub font: &'static FontRef<'static>,
    pub color: ColorConfig,
    pub size: f32,
    /// Horizontal space inserted before this span. Ignored when the
    /// span is the first in its `MultiText`.
    pub gap_before: f32,
}

impl TextSpan {
    /// Body-text span (Liberation Sans Bold), no leading gap.
    pub fn text(content: impl Into<String>, size: f32, color: ColorConfig) -> Self {
        Self {
            content: content.into(),
            font: text_font(),
            color,
            size,
            gap_before: 0.0,
        }
    }

    /// Single weather-icon glyph, no leading gap.
    pub fn icon(glyph: char, size: f32, color: ColorConfig) -> Self {
        Self {
            content: glyph.to_string(),
            font: icon_font(),
            color,
            size,
            gap_before: 0.0,
        }
    }

    /// Builder: set the gap inserted before this span when it's not
    /// first in a `MultiText`.
    pub fn with_gap_before(mut self, gap: f32) -> Self {
        self.gap_before = gap;
        self
    }
}

pub enum GenericDrawable {
    /// One or more spans of text rendered on a *shared baseline*.
    /// One span = a plain text or icon leaf; multiple spans = mixed-
    /// font lines like the today-weather row (icon glyph followed by
    /// a temperature label) where flex baseline alignment across
    /// fonts with different ascent metrics is awkward.
    MultiText(Vec<TextSpan>),
    /// Rounded-rect fill — typically attached to a parent node so it
    /// paints behind the children. Sized by the parent box, not by
    /// the variant itself (`measure` returns ZERO).
    RoundedRect {
        fill_color: ColorConfig,
        radius: f32,
    },
}

impl Drawable for GenericDrawable {
    fn measure(&self) -> Size<f32> {
        match self {
            GenericDrawable::MultiText(spans) => {
                let mut width = 0.0;
                let mut height = 0.0_f32;
                for (i, span) in spans.iter().enumerate() {
                    let scale = PxScale::from(span.size);
                    let scaled = span.font.as_scaled(scale);
                    if i > 0 {
                        width += span.gap_before;
                    }
                    width += line_width(span.font, scale, &span.content);
                    height = height.max(scaled.height());
                }
                Size { width, height }
            }
            GenericDrawable::RoundedRect { .. } => Size::ZERO,
        }
    }

    fn draw(&self, canvas: &mut Pixmap, x: f32, y: f32, w: f32, h: f32) {
        match self {
            GenericDrawable::MultiText(spans) => {
                // Shared baseline = top of the box plus the largest
                // ascent across all spans, so glyphs from different
                // fonts sit on the same imaginary line regardless of
                // their individual heights.
                let baseline = y + spans
                    .iter()
                    .map(|s| s.font.as_scaled(PxScale::from(s.size)).ascent())
                    .fold(0.0_f32, f32::max);
                let mut cursor = x;
                for (i, span) in spans.iter().enumerate() {
                    let scale = PxScale::from(span.size);
                    if i > 0 {
                        cursor += span.gap_before;
                    }
                    draw_line(
                        canvas,
                        span.font,
                        scale,
                        cursor,
                        baseline,
                        &span.content,
                        span.color.to_tiny_skia(),
                        None,
                    );
                    cursor += line_width(span.font, scale, &span.content);
                }
            }
            GenericDrawable::RoundedRect { fill_color, radius } => {
                paint_rounded_rect(canvas, x, y, w, h, *radius, *fill_color);
            }
        }
    }
}
