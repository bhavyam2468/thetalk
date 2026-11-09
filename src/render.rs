use crate::anim::LogicalRect;
use crate::attach::{format_bytes, Attachment, AttachmentStatus};
use crate::blur::{draw_diffuse_rrect_shadow, BlurScratch};
use crate::config::AppConfig;
use crate::markdown::{
    parse_markdown, unpack_frontier_idx, unpack_link_idx, ListMarker, MdBlock, ParsedMarkdown,
    RichSpan, META_CODESPAN, META_LINK, META_MARK, META_STRIKE,
};
use crate::palette::{DropdownKind, PaletteRoute, MAX_VISIBLE_ROWS, ROW_HEIGHT, ROW_STEP};
use crate::presentation::{PanelSnapshot, PanelTransition, TRANSITION_SECONDS};
use crate::state::{
    AppState, ChatItem, FocusTarget, HitTarget, HitZone, Panel, Role, TextField, BAR_MIN_HEIGHT,
    SURFACE_HEIGHT, SURFACE_WIDTH,
};
use crate::stream::WordFocusState;
use crate::theme::{GrainLut, Rgba, Theme, ThemeMode};
use cosmic_text::{
    fontdb, Attrs, Buffer, Color as CosmicColor, Family, FontSystem, Metrics, Shaping, Style,
    SwashCache, SwashContent, Weight, Wrap,
};
use std::path::Path;
use std::rc::Rc;
use std::hash::{Hash, Hasher};
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Mask, Paint, PathBuilder, Pixmap, Stroke, Transform,
};

static FONT_GEIST_REGULAR: &[u8] = include_bytes!("../assets/fonts/Geist-Regular.ttf");
static FONT_GEIST_MEDIUM: &[u8] = include_bytes!("../assets/fonts/Geist-Medium.ttf");
static FONT_GEIST_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Geist-SemiBold.ttf");
static FONT_GEIST_BOLD: &[u8] = include_bytes!("../assets/fonts/Geist-Bold.ttf");
static FONT_GEIST_ITALIC: &[u8] = include_bytes!("../assets/fonts/Geist-Italic.ttf");
static FONT_GEIST_MONO_REGULAR: &[u8] = include_bytes!("../assets/fonts/GeistMono-Regular.ttf");
static FONT_GEIST_MONO_MEDIUM: &[u8] = include_bytes!("../assets/fonts/GeistMono-Medium.ttf");

struct ShadowTemplate {
    key: [u32; 3],
    margin: f32,
    corner: f32,
    pixmap: Pixmap,
}

pub struct Renderer {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    pub blur_scratch: BlurScratch,
    pub grain: GrainLut,
    pub pixmap: Pixmap,
    pub mask: Mask,
    pub scale: f32,
    panel_transition: PanelTransition,
    last_above_rect: Option<(usize, usize, usize, usize)>,
    outgoing_panel: Option<PanelSnapshot>,
    shadow_cache: Option<ShadowTemplate>,
    glyph_placements: Vec<(cosmic_text::CacheKey, i32, i32, u32, u32)>,
    message_cache: Vec<(usize, u64, Rc<ShapedMessage>)>,
}

#[derive(Clone, Copy)]
struct ClipBox {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl ClipBox {
    fn intersect(self, other: Self) -> Self {
        Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        }
    }

    fn as_tuple(self) -> (i32, i32, i32, i32) {
        (self.x0, self.y0, self.x1, self.y1)
    }
}

#[derive(Clone, Copy)]
struct DrawContext {
    scale: f32,
    clip: ClipBox,
    alpha_mul: f32,
}

enum ShapedBlock {
    Text {
        buffer: Buffer,
        height: f32,
        indent_x: f32,
        quote_bar: bool,
        marker: Option<ListMarker>,
        top_gap: f32,
        msg_idx: usize,
    },
    Code {
        lang_buf: Option<Buffer>,
        copy_buf: Buffer,
        code_buf: Buffer,
        code_h: f32,
        header_h: f32,
        top_gap: f32,
        msg_idx: usize,
        block_idx: usize,
        copied: bool,
    },
    Table {
        col_widths: Vec<f32>,
        header_cells: Vec<Buffer>,
        header_h: f32,
        body_rows: Vec<(Vec<Buffer>, f32)>,
        top_gap: f32,
        msg_idx: usize,
        block_idx: usize,
    },
    Rule {
        top_gap: f32,
    },
}

struct ShapedMessage {
    msg_idx: usize,
    role: Role,
    user_buf: Option<(Buffer, f32, f32)>,
    user_chips: Vec<(String, f32)>,
    thinking_pill: Option<(Buffer, Option<(Buffer, f32)>)>,
    blocks: Vec<ShapedBlock>,
    waiting_ttft: bool,
    error_buf: Option<(Buffer, f32)>,
    total_height: f32,
}

impl ShapedMessage {
    fn height(&self, state: &AppState) -> f32 {
        let hidden = self.thinking_pill.as_ref().and_then(|(_, body)| body.as_ref())
            .map(|(_, h)| (h + 10.0) * (1.0 - state.messages[self.msg_idx].thinking_reveal.value.clamp(0.0, 1.0)))
            .unwrap_or(0.0);
        (self.total_height - hidden).max(22.0)
    }
}

impl Renderer {
    pub fn new(scale: f32) -> Self {
        let scale = scale.clamp(1.0, 4.0);
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        for font_bytes in [
            FONT_GEIST_REGULAR,
            FONT_GEIST_MEDIUM,
            FONT_GEIST_SEMIBOLD,
            FONT_GEIST_BOLD,
            FONT_GEIST_ITALIC,
            FONT_GEIST_MONO_REGULAR,
            FONT_GEIST_MONO_MEDIUM,
        ] {
            db.load_font_data(font_bytes.to_vec());
        }
        db.set_sans_serif_family("Geist");
        db.set_monospace_family("Geist Mono");

        let font_system = FontSystem::new_with_locale_and_db("en-US".to_string(), db);
        let pw = ((SURFACE_WIDTH as f32) * scale).ceil() as u32;
        let ph = ((SURFACE_HEIGHT as f32) * scale).ceil() as u32;

        Self {
            font_system,
            swash_cache: SwashCache::new(),
            blur_scratch: BlurScratch::new(),
            grain: GrainLut::new(),
            pixmap: Pixmap::new(pw.max(1), ph.max(1)).expect("pixmap alloc"),
            mask: Mask::new(pw.max(1), ph.max(1)).expect("mask alloc"),
            scale,
            panel_transition: PanelTransition::new(),
            last_above_rect: None,
            outgoing_panel: None,
            message_cache: Vec::new(),
            shadow_cache: None,
            glyph_placements: Vec::with_capacity(64),
        }
    }

    pub fn ensure_size(&mut self, logical_w: u32, logical_h: u32, scale: f32) {
        let scale = scale.clamp(1.0, 4.0);
        let pw = ((logical_w as f32) * scale).ceil().max(1.0) as u32;
        let ph = ((logical_h as f32) * scale).ceil().max(1.0) as u32;
        self.scale = scale;
        if self.pixmap.width() != pw || self.pixmap.height() != ph {
            self.pixmap = Pixmap::new(pw, ph).expect("pixmap resize");
            self.mask = Mask::new(pw, ph).expect("mask resize");
            self.last_above_rect = None;
            self.outgoing_panel = None;
            self.shadow_cache = None;
        }
    }

    fn capture_last_above_snapshot(&mut self) -> Option<PanelSnapshot> {
        let (x0, y0, x1, y1) = self.last_above_rect?;
        let pw = self.pixmap.width() as usize;
        let ph = self.pixmap.height() as usize;
        let x0 = x0.min(pw);
        let x1 = x1.min(pw);
        let y0 = y0.min(ph);
        let y1 = y1.min(ph);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let w = x1 - x0;
        let h = y1 - y0;
        let src = self.pixmap.data();
        let mut pixels = Vec::with_capacity(w * h * 4);
        for y in y0..y1 {
            pixels.extend_from_slice(&src[(y * pw + x0) * 4..(y * pw + x1) * 4]);
        }
        Some(PanelSnapshot {
            width: w,
            height: h,
            pixels,
        })
    }

    pub fn render(&mut self, state: &mut AppState) {
        let now = state.now_secs();
        let route = state.route_key();
        if self.panel_transition.update_route(route, now) {
            self.outgoing_panel = self.capture_last_above_snapshot();
            state.panel_transition_until = now + TRANSITION_SECONDS;
        }
        let panel_progress = self.panel_transition.progress(now);
        if panel_progress >= 1.0 {
            self.outgoing_panel = None;
        }

        self.last_above_rect = None;
        self.pixmap.fill(Color::TRANSPARENT);
        state.hit_zones.clear();
        state.scroll_regions.clear();
        state.palette_viewport_rect = None;

        let presence = state.presence_spring.value.clamp(0.0, 1.0);
        if !state.visible && presence <= 0.01 {
            return;
        }

        let scale = self.scale;
        let theme = state.theme;
        let panel = state.panel();

        // 1. Shape composer text + attachment chips to compute exact dynamic bar height
        let expanded = state.is_expanded();
        let target_w = if expanded {
            crate::state::BAR_WIDTH_EXPANDED
        } else {
            crate::state::BAR_WIDTH_COMPACT
        };
        let send_visible = state.send_bloom.value > 0.03;
        let composer_pad_r = if send_visible { 50.0 } else { 18.0 };
        let composer_wrap_w = (target_w - 18.0 - composer_pad_r).max(120.0);
        let (composer_buf, composer_text_h) = self.shape_plain_field(
            &state.composer.text,
            14.5,
            22.0,
            composer_wrap_w,
            theme.fg,
            false,
            Wrap::WordOrGlyph,
        );
        let chips_h = self.measure_composer_chips_height(state, target_w - 36.0);
        let desired_composer_h = (composer_text_h + 24.0 + chips_h).clamp(BAR_MIN_HEIGHT, 210.0);
        state.desired_composer_height = desired_composer_h;

        // 2. Shape above-bar content (Settings, Command Palette, or Conversation messages)
        // Measure at the destination width, not a moving spring value. Otherwise
        // line wrapping continually retargets height during a width morph.
        let inner_w = (target_w - 44.0).max(200.0);
        let mut shaped_msgs: Vec<Rc<ShapedMessage>> = Vec::new();
        let mut messages_total_h = 0.0f32;

        if panel == Panel::Settings {
            state.desired_content_height = self.estimate_settings_height(state);
        } else if panel == Panel::Commands {
            state.desired_content_height = self.estimate_palette_height(state);
        } else if panel == Panel::Conversation {
            let mut y_acc = 18.0f32;
            for idx in 0..state.messages.len() {
                let shaped = self.cached_message(
                    &state.messages[idx],
                    idx,
                    inner_w,
                    now,
                    &theme,
                    state.copied_badge,
                );
                y_acc += shaped.height(state) + 18.0;
                shaped_msgs.push(shaped);
            }
            messages_total_h = y_acc + 4.0;
            state.desired_content_height = messages_total_h;
        } else {
            state.desired_content_height = 0.0;
        }

        state.sync_geometry_targets();

        // 3. Current spring-animated card bounds
        let mut card = state.card_rect.current();
        card.y += (1.0 - presence) * 14.0;
        let radius = state.card_radius.value.clamp(8.0, 28.0);

        self.draw_card_shadow(card, radius, presence, &theme);

        // 5. Card clip mask + Quiet Paper `float` fill + monochrome analog film grain
        self.mask.clear();
        if let Some(card_path) = build_rrect_path(
            card.x * scale,
            card.y * scale,
            card.w * scale,
            card.h * scale,
            radius * scale,
        ) {
            self.mask
                .fill_path(&card_path, FillRule::Winding, true, Transform::identity());

            let mut bg_paint = Paint::default();
            let bg = theme.float.with_alpha(presence);
            bg_paint.set_color_rgba8(bg.r, bg.g, bg.b, bg.a);
            bg_paint.anti_alias = true;
            self.pixmap.fill_path(
                &card_path,
                &bg_paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }

        self.apply_card_grain(card, theme.grain * presence);

        // 6. Split [Above-bar panel] / [Bottom omnibox input bar]
        let actual_composer_h = desired_composer_h.min(card.h);
        let composer_top = card.y + card.h - actual_composer_h;
        let above_h = (composer_top - card.y).max(0.0);

        if above_h > 4.0 {
            let content_fade = panel_progress;
            let dir = self.panel_transition.direction() as f32;
            let incoming_slide_x = if panel == Panel::Commands {
                dir * 10.0 * (1.0 - panel_progress)
            } else {
                0.0
            };
            let above_clip = ClipBox {
                x0: ((card.x + 1.0) * scale).floor() as i32,
                y0: ((card.y + 1.0) * scale).floor() as i32,
                x1: ((card.x + card.w - 1.0) * scale).ceil() as i32,
                y1: (composer_top * scale).floor() as i32,
            };
            self.restrict_mask(above_clip);
            let ctx = DrawContext {
                scale,
                clip: above_clip,
                alpha_mul: presence * content_fade,
            };

            if panel == Panel::Settings {
                self.draw_settings_panel(state, card.x, card.y, card.w, above_h, ctx);
            } else if panel == Panel::Commands {
                self.draw_palette_panel(
                    state,
                    card.x + incoming_slide_x,
                    card.y,
                    composer_top,
                    card.w,
                    above_h,
                    ctx,
                );
            } else if !shaped_msgs.is_empty() {
                let viewport_h = above_h;
                state.max_scroll = (messages_total_h - viewport_h).max(0.0);
                if !state.user_scrolled_up && state.is_streaming() {
                    state.scroll_spring.retarget(state.max_scroll);
                }
                let scroll_y = state.scroll_spring.value.clamp(0.0, state.max_scroll.max(0.0));
                self.draw_messages_viewport(
                    state,
                    &shaped_msgs,
                    card.x,
                    card.y,
                    card.w,
                    viewport_h,
                    scroll_y,
                    ctx,
                );
            }

            state.hit_zones.retain_mut(|zone| {
                let x0 = zone.rect.x.max(above_clip.x0 as f32 / scale);
                let y0 = zone.rect.y.max(above_clip.y0 as f32 / scale);
                let x1 = (zone.rect.x + zone.rect.w).min(above_clip.x1 as f32 / scale);
                let y1 = (zone.rect.y + zone.rect.h).min(above_clip.y1 as f32 / scale);
                zone.rect = LogicalRect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0));
                x1 > x0 && y1 > y0
            });

            // Every route uses the same outgoing-content retention. Reversals
            // capture the currently composited scene right before clearing the frame.
            self.retain_panel(card, composer_top, panel_progress, presence);

            // Whisper-thin hairline divider above composer bar
            self.fill_rect_logical(
                card.x + 14.0,
                composer_top - 0.5,
                card.w - 28.0,
                1.0,
                theme.line.with_alpha(presence),
                true,
            );
        } else {
            state.max_scroll = 0.0;
        }

        // Restore rounded card clip after the panel's stricter viewport.
        self.mask.clear();
        if let Some(path) = build_rrect_path(card.x * scale, card.y * scale,
            card.w * scale, card.h * scale, radius * scale) {
            self.mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
        }

        // 7. Composer Input Bar (zero placeholders, zero tooltips, pure finished surface)
        self.draw_composer_bar(
            state,
            &composer_buf,
            composer_text_h,
            LogicalRect::new(card.x, composer_top, card.w, actual_composer_h),
            presence,
            now,
        );

        // 8. Subtle top-edge specular inset & crisp 1px outer card border
        self.fill_rect_logical(
            card.x + radius * 0.65,
            card.y + 0.5,
            (card.w - radius * 1.3).max(0.0),
            1.0,
            theme.specular_inset.with_alpha(presence),
            true,
        );
        let outer_border = if state.has_keyboard_focus {
            Rgba::lerp_premultiplied(theme.line, theme.accent.with_alpha(0.52), 0.55)
                .with_alpha(presence)
        } else {
            theme.line.with_alpha(presence)
        };
        self.stroke_rrect_logical(card.x, card.y, card.w, card.h, radius, 1.0, outer_border);
    }

    fn draw_card_shadow(
        &mut self,
        card: LogicalRect,
        radius: f32,
        presence: f32,
        theme: &Theme,
    ) {
        if presence <= 0.005 || theme.shadow_alpha <= 0.001 {
            return;
        }

        let scale = self.scale;
        let margin = 38.0f32;
        let template_corner = 36.0f32;
        let key = [
            radius.to_bits(),
            (scale * 1000.0).round() as u32,
            theme.shadow_alpha.to_bits(),
        ];

        let needs_rebuild = match &self.shadow_cache {
            Some(cached) => cached.key != key,
            None => true,
        };

        if needs_rebuild {
            let temp_card_w = template_corner * 2.0 + 4.0;
            let temp_card_h = template_corner * 2.0 + 4.0;
            let tw = ((temp_card_w + margin * 2.0) * scale).ceil().max(1.0) as u32;
            let th = ((temp_card_h + margin * 2.0) * scale).ceil().max(1.0) as u32;

            if let Some(mut shadow_pixmap) = Pixmap::new(tw, th) {
                for (offset, sigma, alpha) in [
                    (3.0, 8.0, theme.shadow_alpha),
                    (1.0, 2.5, theme.shadow_alpha * 0.42),
                ] {
                    draw_diffuse_rrect_shadow(
                        &mut shadow_pixmap.as_mut(),
                        margin * scale,
                        margin * scale,
                        temp_card_w * scale,
                        temp_card_h * scale,
                        radius * scale,
                        offset * scale,
                        sigma * scale,
                        alpha,
                        (0, 0, 0),
                    );
                }
                self.shadow_cache = Some(ShadowTemplate {
                    key,
                    margin,
                    corner: template_corner,
                    pixmap: shadow_pixmap,
                });
            }
        }

        let (pixmap, template) = match (&mut self.pixmap, &self.shadow_cache) {
            (p, Some(t)) => (p, t),
            _ => return,
        };

        let margin = template.margin;
        let corner = template.corner.min(card.w * 0.45).min(card.h * 0.45);
        let p_margin = (margin * scale).round() as i32;
        let p_corner = (corner * scale).round() as i32;

        let dst_x = ((card.x - margin) * scale).round() as i32;
        let dst_y = ((card.y - margin) * scale).round() as i32;
        let dst_w = ((card.w + margin * 2.0) * scale).round() as i32;
        let dst_h = ((card.h + margin * 2.0) * scale).round() as i32;

        let tw = template.pixmap.width();
        let th = template.pixmap.height();

        let cw = (p_margin + p_corner).max(0) as u32;
        let ch = (p_margin + p_corner).max(0) as u32;

        // 1. Top-left corner
        blit_sub_pixmap(pixmap, dst_x, dst_y, &template.pixmap, 0, 0, cw, ch, presence);

        // 2. Top-right corner
        blit_sub_pixmap(pixmap, dst_x + dst_w - cw as i32, dst_y, &template.pixmap, tw - cw, 0, cw, ch, presence);

        // 3. Bottom-left corner
        blit_sub_pixmap(pixmap, dst_x, dst_y + dst_h - ch as i32, &template.pixmap, 0, th - ch, cw, ch, presence);

        // 4. Bottom-right corner
        blit_sub_pixmap(pixmap, dst_x + dst_w - cw as i32, dst_y + dst_h - ch as i32, &template.pixmap, tw - cw, th - ch, cw, ch, presence);

        // 5. Top edge (stretched horizontally)
        let mid_w = (dst_w - (cw as i32 * 2)).max(0) as u32;
        if mid_w > 0 {
            let src_sample_x = tw / 2;
            blit_stretched_horizontal(pixmap, dst_x + cw as i32, dst_y, mid_w, &template.pixmap, src_sample_x, 0, ch, presence);

            // 6. Bottom edge (stretched horizontally)
            blit_stretched_horizontal(pixmap, dst_x + cw as i32, dst_y + dst_h - ch as i32, mid_w, &template.pixmap, src_sample_x, th - ch, ch, presence);
        }

        // 7. Left & Right edges (stretched vertically)
        let mid_h = (dst_h - (ch as i32 * 2)).max(0) as u32;
        if mid_h > 0 {
            let src_sample_y = th / 2;
            blit_stretched_vertical(pixmap, dst_x, dst_y + ch as i32, mid_h, &template.pixmap, 0, src_sample_y, cw, presence);
            blit_stretched_vertical(pixmap, dst_x + dst_w - cw as i32, dst_y + ch as i32, mid_h, &template.pixmap, tw - cw, src_sample_y, cw, presence);
        }
    }

    /// Composite the outgoing panel bottom-centered inside the current rounded
    /// card and remember this frame's live above-composer region for interruption-safe
    /// snapshot capture before the next clear.
    fn retain_panel(
        &mut self,
        card: LogicalRect,
        composer_top: f32,
        panel_progress: f32,
        presence: f32,
    ) {
        let scale = self.scale;
        let pw = self.pixmap.width() as usize;
        let ph = self.pixmap.height() as usize;
        let x0 = (card.x * scale).ceil().max(0.0) as usize;
        let x1 = ((card.x + card.w) * scale).floor().max(0.0).min(pw as f32) as usize;
        let y0 = (card.y * scale).ceil().max(0.0) as usize;
        let y1 = (composer_top * scale).floor().max(0.0).min(ph as f32) as usize;
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let w = x1 - x0;
        let outgoing_alpha = (1.0 - panel_progress) * presence;
        let dir = self.panel_transition.direction() as f32;
        if outgoing_alpha > 0.002 {
            if let Some(old) = &self.outgoing_panel {
                let slide_px = (-dir * 10.0 * panel_progress * scale).round() as i32;
                let ox = (x0 + w / 2) as i32 - (old.width / 2) as i32 + slide_px;
                let oy = y1 as i32 - old.height as i32;
                if let Some(source) =
                    tiny_skia::PixmapRef::from_bytes(&old.pixels, old.width as u32, old.height as u32)
                {
                    self.pixmap.draw_pixmap(
                        ox,
                        oy,
                        source,
                        &tiny_skia::PixmapPaint {
                            opacity: outgoing_alpha.clamp(0.0, 1.0),
                            ..Default::default()
                        },
                        Transform::identity(),
                        Some(&self.mask),
                    );
                }
            }
        }
        self.last_above_rect = Some((x0, y0, x1, y1));
    }

    fn restrict_mask(&mut self, clip: ClipBox) {
        let w = self.mask.width() as usize;
        let h = self.mask.height() as usize;
        let x0 = clip.x0.clamp(0, w as i32) as usize;
        let x1 = clip.x1.clamp(x0 as i32, w as i32) as usize;
        for (y, row) in self.mask.data_mut().chunks_exact_mut(w).take(h).enumerate() {
            if (y as i32) < clip.y0 || (y as i32) >= clip.y1 { row.fill(0); }
            else { row[..x0].fill(0); row[x1..].fill(0); }
        }
    }

    fn apply_card_grain(&mut self, card: LogicalRect, grain_amount: f32) {
        if grain_amount <= 0.002 {
            return;
        }
        let pw = self.pixmap.width() as i32;
        let ph = self.pixmap.height() as i32;
        let x0 = ((card.x * self.scale).floor() as i32).clamp(0, pw);
        let y0 = ((card.y * self.scale).floor() as i32).clamp(0, ph);
        let x1 = (((card.x + card.w) * self.scale).ceil() as i32).clamp(0, pw);
        let y1 = (((card.y + card.h) * self.scale).ceil() as i32).clamp(0, ph);
        let scale_amp = (grain_amount * 115.0).round() as i32;
        if scale_amp <= 0 {
            return;
        }

        let mask_data = self.mask.data();
        let dst = self.pixmap.data_mut();
        let stride = pw as usize;

        for py in y0..y1 {
            let row = (py as usize) * stride;
            let lut_row = ((py as usize) & 127) << 7;
            for px in x0..x1 {
                let idx = row + px as usize;
                let m = mask_data[idx];
                if m < 200 {
                    continue;
                }
                let p = idx * 4;
                if dst[p + 3] < 240 {
                    continue;
                }
                let n = ((self.grain.sample_fast(lut_row, px as usize) as i32) * scale_amp) >> 6;
                dst[p] = (dst[p] as i32 + n).clamp(0, dst[p + 3] as i32) as u8;
                dst[p + 1] = (dst[p + 1] as i32 + n).clamp(0, dst[p + 3] as i32) as u8;
                dst[p + 2] = (dst[p + 2] as i32 + n).clamp(0, dst[p + 3] as i32) as u8;
            }
        }
    }

    fn accent_ink(theme: &Theme) -> Rgba {
        match theme.mode {
            ThemeMode::Dark => Rgba::hex(0x1b1917),
            ThemeMode::Light => Rgba::hex(0xfaf7f2),
        }
    }

    fn code_card_bg(theme: &Theme) -> Rgba {
        match theme.mode {
            ThemeMode::Dark => Rgba::hex(0x1b1a18),
            ThemeMode::Light => Rgba::hex(0xede8de),
        }
    }

    fn chip_label_for_pending(att: &Attachment) -> String {
        let short_name = truncate_label(&att.name, 20);
        match &att.status {
            AttachmentStatus::Loading => format!("...  {}", short_name),
            AttachmentStatus::Error(_) => format!("ERR  {}", short_name),
            AttachmentStatus::Ready => {
                let tag = att.kind.badge();
                if let Some(pages) = att.page_count {
                    format!("{}  {} · {}p", tag, short_name, pages)
                } else {
                    format!("{}  {} · {}", tag, short_name, format_bytes(att.size_bytes))
                }
            }
        }
    }

    fn measure_composer_chips_height(&mut self, state: &AppState, avail_w: f32) -> f32 {
        if state.composer_attachments.is_empty() {
            return 0.0;
        }
        let mut x = 0.0f32;
        let mut rows = 1usize;
        for att in &state.composer_attachments {
            let lbl = Self::chip_label_for_pending(att);
            let (buf, _) =
                self.shape_plain_field(&lbl, 11.2, 15.0, avail_w, state.theme.fg, false, Wrap::None);
            let cw = (measure_buffer_width(&buf) + 28.0).min(avail_w);
            if x > 0.0 && x + cw > avail_w {
                rows += 1;
                x = 0.0;
            }
            x += cw + 6.0;
        }
        (rows as f32) * 24.0 + ((rows.saturating_sub(1)) as f32) * 6.0 + 8.0
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_composer_bar(
        &mut self,
        state: &mut AppState,
        composer_buf: &Buffer,
        composer_text_h: f32,
        bar_rect: LogicalRect,
        presence: f32,
        now: f64,
    ) {
        let scale = self.scale;
        let theme = state.theme;

        self.fill_rrect_logical(
            bar_rect.x,
            bar_rect.y,
            bar_rect.w,
            bar_rect.h,
            16.0,
            theme.float.with_alpha(presence),
            true,
        );

        state.hit_zones.push(HitZone {
            rect: LogicalRect::new(
                bar_rect.x + 6.0,
                bar_rect.y + 3.0,
                bar_rect.w - 12.0,
                bar_rect.h - 6.0,
            ),
            target: HitTarget::Input(FocusTarget::Composer),
        });

        let bar_clip = ClipBox {
            x0: ((bar_rect.x + 10.0) * scale).floor() as i32,
            y0: ((bar_rect.y + 4.0) * scale).floor() as i32,
            x1: ((bar_rect.x + bar_rect.w - 10.0) * scale).ceil() as i32,
            y1: ((bar_rect.y + bar_rect.h - 4.0) * scale).ceil() as i32,
        };
        let ctx = DrawContext {
            scale,
            clip: bar_clip,
            alpha_mul: presence,
        };

        let mut chips_offset_y = 0.0f32;
        if !state.composer_attachments.is_empty() {
            let start_x = bar_rect.x + 14.0;
            let avail_w = (bar_rect.w - 28.0).max(120.0);
            let mut cx = start_x;
            let mut cy = bar_rect.y + 8.0;
            let att_snapshot: Vec<(String, f32)> = state
                .composer_attachments
                .iter()
                .map(|a| (Self::chip_label_for_pending(a), a.entrance.value.clamp(0.0, 1.0)))
                .collect();

            for (idx, (lbl, pop)) in att_snapshot.into_iter().enumerate() {
                let (buf, _) =
                    self.shape_plain_field(&lbl, 11.2, 15.0, avail_w - 26.0, theme.fg, false, Wrap::None);
                let cw = (measure_buffer_width(&buf) + 28.0).min(avail_w);
                if cx > start_x && cx + cw > start_x + avail_w {
                    cx = start_x;
                    cy += 30.0;
                }
                let chip_y = cy + (1.0 - pop) * 3.0;
                let crect = LogicalRect::new(cx, chip_y, cw, 24.0);
                let hovered = matches!(
                    state.hovered_hit(),
                    Some(HitTarget::ComposerAttachChipRemove(i)) if i == idx
                );
                state.hit_zones.push(HitZone {
                    rect: LogicalRect::new(crect.x + crect.w - 22.0, crect.y, 22.0, crect.h),
                    target: HitTarget::ComposerAttachChipRemove(idx),
                });
                let chip_alpha = ctx.alpha_mul * pop;
                self.fill_rrect_logical(
                    crect.x,
                    crect.y,
                    crect.w,
                    crect.h,
                    7.0,
                    if hovered { theme.surface_hover } else { theme.surface }.with_alpha(chip_alpha),
                    true,
                );
                self.stroke_rrect_logical(
                    crect.x,
                    crect.y,
                    crect.w,
                    crect.h,
                    7.0,
                    1.0,
                    theme.line.with_alpha(chip_alpha),
                );
                let chip_ctx = DrawContext {
                    alpha_mul: chip_alpha,
                    ..ctx
                };
                self.draw_buffer_runs(&buf, crect.x + 8.0, crect.y + 4.5, &[], None, state, chip_ctx);
                let (xbuf, _) = self.shape_plain_field(
                    "×",
                    12.0,
                    15.0,
                    14.0,
                    if hovered { theme.fg } else { theme.muted },
                    false,
                    Wrap::None,
                );
                self.draw_buffer_runs(&xbuf, crect.x + crect.w - 15.0, crect.y + 4.0, &[], None, state, chip_ctx);
                cx += cw + 6.0;
            }
            chips_offset_y = (cy + 24.0 - bar_rect.y) + 4.0;
        }

        let text_x = bar_rect.x + 18.0;
        let text_y = if chips_offset_y > 0.0 {
            bar_rect.y + chips_offset_y + 4.0
        } else {
            bar_rect.y + ((bar_rect.h - composer_text_h) * 0.5).max(11.0)
        };

        let show_composer_cursor = state.has_keyboard_focus
            && state.focus_target == FocusTarget::Composer
            && (!state.slash_menu_open() || (!state.palette.open && state.composer_has_slash_trigger()));
        if show_composer_cursor {
            self.draw_field_selection_and_cursor(
                &state.composer,
                composer_buf,
                text_x,
                text_y,
                22.0,
                theme,
                now - state.last_cursor_activity,
                ctx,
            );
        }

        if !state.composer.text.is_empty() {
            self.draw_buffer_runs(composer_buf, text_x, text_y, &[], None, state, ctx);
        }

        // Transient status whisper (e.g. clipboard error, attachment limit, copied)
        if let Some((status_txt, exp)) = state.attach_status.clone() {
            if now < exp {
                let fade = ((exp - now).min(0.4) / 0.4) as f32;
                let (sbuf, _) = self.shape_plain_field(
                    &status_txt,
                    11.2,
                    15.0,
                    260.0,
                    theme.muted,
                    false,
                    Wrap::None,
                );
                let sw = measure_buffer_width(&sbuf);
                let right_edge = if state.send_bloom.value > 0.05 {
                    bar_rect.x + bar_rect.w - 46.0
                } else {
                    bar_rect.x + bar_rect.w - 16.0
                };
                if sw + 30.0 < bar_rect.w * 0.55 {
                    self.draw_buffer_runs(
                        &sbuf,
                        right_edge - sw,
                        bar_rect.y + bar_rect.h - 29.0,
                        &[],
                        None,
                        state,
                        DrawContext {
                            alpha_mul: presence * fade,
                            ..ctx
                        },
                    );
                }
            }
        }

        // Send / Stop circular action button (blooms smoothly only when needed)
        let bloom = state.send_bloom.value.clamp(0.0, 1.0);
        if bloom > 0.02 {
            let btn_size = 28.0 * (0.72 + 0.28 * bloom);
            let btn_cx = bar_rect.x + bar_rect.w - 23.0;
            let btn_cy = bar_rect.y + bar_rect.h - 23.0;
            let btn_rect = LogicalRect::new(
                btn_cx - btn_size * 0.5,
                btn_cy - btn_size * 0.5,
                btn_size,
                btn_size,
            );
            state.hit_zones.push(HitZone {
                rect: LogicalRect::new(btn_cx - 17.0, btn_cy - 17.0, 34.0, 34.0),
                target: HitTarget::SendOrStop,
            });

            let hovered = matches!(state.hovered_hit(), Some(HitTarget::SendOrStop));
            let btn_bg = theme
                .accent
                .with_alpha(presence * bloom * if hovered { 0.88 } else { 1.0 });
            self.fill_rrect_logical(
                btn_rect.x,
                btn_rect.y,
                btn_rect.w,
                btn_rect.h,
                btn_size * 0.5,
                btn_bg,
                true,
            );

            let fg = Self::accent_ink(&theme).with_alpha(presence * bloom);
            if state.is_streaming() {
                let sq = 8.0 * (0.8 + 0.2 * bloom);
                self.fill_rrect_logical(
                    btn_cx - sq * 0.5,
                    btn_cy - sq * 0.5,
                    sq,
                    sq,
                    2.0,
                    fg,
                    true,
                );
            } else {
                self.draw_up_arrow_icon(btn_cx, btn_cy, 5.2 * (0.8 + 0.2 * bloom), fg);
            }
        }
    }

    fn palette_shows_header(&self, state: &AppState) -> bool {
        state.palette.open
            || state.palette.stack.len() > 1
            || state.palette.dropdown.is_some()
            || state.palette.has_search_bar()
    }

    fn estimate_palette_height(&self, state: &AppState) -> f32 {
        let header_h = if self.palette_shows_header(state) {
            32.0
        } else {
            0.0
        };
        let search_h = if state.palette.has_search_bar() {
            38.0
        } else {
            0.0
        };
        let count = if let Some(dd) = state.palette.dropdown {
            state
                .palette
                .build_dropdown_options(dd, &state.config)
                .len()
        } else {
            state.palette_rows().len()
        };
        let vis = count.max(1).min(MAX_VISIBLE_ROWS) as f32;
        header_h + search_h + vis * ROW_STEP + 14.0
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_palette_panel(
        &mut self,
        state: &mut AppState,
        card_x: f32,
        _card_y: f32,
        composer_top: f32,
        card_w: f32,
        _above_h: f32,
        ctx: DrawContext,
    ) {
        let theme = state.theme;
        let now = state.now_secs();
        let focus_border = Rgba::lerp_premultiplied(theme.line, theme.accent, 0.65);

        let show_header = self.palette_shows_header(state);
        let show_search = state.palette.has_search_bar();
        let header_h = if show_header { 32.0 } else { 0.0 };
        let search_h = if show_search { 38.0 } else { 0.0 };

        let content_h = self.estimate_palette_height(state);
        let mut y = composer_top - content_h + 8.0;

        // 1. Optional Breadcrumb Header
        if show_header {
            let can_back = state.palette.stack.len() > 1 || state.palette.dropdown.is_some();
            if can_back {
                let back_rect = LogicalRect::new(card_x + 12.0, y, 24.0, 24.0);
                state.hit_zones.push(HitZone {
                    rect: back_rect,
                    target: HitTarget::PaletteBack,
                });
                let bhov = matches!(state.hovered_hit(), Some(HitTarget::PaletteBack));
                self.fill_rrect_logical(
                    back_rect.x,
                    back_rect.y,
                    back_rect.w,
                    back_rect.h,
                    6.0,
                    if bhov { theme.surface_hover } else { theme.surface }.with_alpha(ctx.alpha_mul),
                    true,
                );
                let (bbuf, _) =
                    self.shape_plain_field("←", 12.5, 16.0, 20.0, theme.fg, false, Wrap::None);
                self.draw_buffer_runs(&bbuf, back_rect.x + 6.0, back_rect.y + 3.5, &[], None, state, ctx);
            }

            let crumbs = state.palette.breadcrumbs().join("  /  ");
            let crumb_x = card_x + if can_back { 42.0 } else { 18.0 };
            let (cbuf, _) = self.shape_plain_field(
                &crumbs,
                12.2,
                16.0,
                card_w - 150.0,
                theme.muted,
                false,
                Wrap::None,
            );
            self.draw_buffer_runs(&cbuf, crumb_x, y + 4.0, &[], None, state, ctx);

            let hint = if state.palette.dropdown.is_some() {
                "← back · Enter select"
            } else if state.palette.editing_field.is_some() {
                "Enter save · Tab next"
            } else if matches!(state.palette.current_route(), Some(PaletteRoute::Provider)) {
                "Enter edit · ← back"
            } else {
                "← back · Esc close"
            };
            let (hbuf, _) = self.shape_plain_field(
                hint,
                11.0,
                15.0,
                160.0,
                theme.faint,
                false,
                Wrap::None,
            );
            let hw = measure_buffer_width(&hbuf);
            self.draw_buffer_runs(
                &hbuf,
                card_x + card_w - hw - 16.0,
                y + 4.5,
                &[],
                None,
                state,
                ctx,
            );
            y += header_h;
        }

        // 2. Optional Submenu Search / Path Box
        if show_search && state.palette.dropdown.is_none() {
            let srect = LogicalRect::new(card_x + 12.0, y, card_w - 24.0, 32.0);
            state.hit_zones.push(HitZone {
                rect: srect,
                target: HitTarget::PaletteSearchBox,
            });
            self.fill_rrect_logical(
                srect.x,
                srect.y,
                srect.w,
                srect.h,
                8.0,
                theme.surface.with_alpha(ctx.alpha_mul),
                true,
            );
            self.stroke_rrect_logical(
                srect.x,
                srect.y,
                srect.w,
                srect.h,
                8.0,
                1.0,
                focus_border.with_alpha(ctx.alpha_mul * 0.75),
            );

            let query_field = state
                .palette
                .current_frame()
                .map(|f| f.query.clone())
                .unwrap_or_default();
            let s_clip = ClipBox {
                x0: ((srect.x + 8.0) * ctx.scale).floor() as i32,
                y0: ((srect.y + 3.0) * ctx.scale).floor() as i32,
                x1: ((srect.x + srect.w - 8.0) * ctx.scale).ceil() as i32,
                y1: ((srect.y + srect.h - 3.0) * ctx.scale).ceil() as i32,
            }
            .intersect(ctx.clip);
            let sctx = DrawContext {
                clip: s_clip,
                ..ctx
            };

            if query_field.text.is_empty() {
                let placeholder = match state.palette.current_route() {
                    Some(PaletteRoute::Models) => "Filter models or type custom model ID…",
                    Some(PaletteRoute::Attach) => "Filter folders or enter ~/path/to/file…",
                    Some(PaletteRoute::FileBrowser(_)) => "Filter files in folder…",
                    _ => "Filter options…",
                };
                let (pbuf, _) = self.shape_plain_field(
                    placeholder,
                    12.5,
                    18.0,
                    srect.w - 20.0,
                    theme.faint,
                    false,
                    Wrap::None,
                );
                self.draw_buffer_runs(&pbuf, srect.x + 10.0, srect.y + 7.0, &[], None, state, sctx);
            }

            let (qbuf, _) = self.shape_plain_field(
                &query_field.text,
                13.0,
                18.0,
                srect.w - 20.0,
                theme.fg,
                false,
                Wrap::None,
            );
            if state.has_keyboard_focus {
                self.draw_field_selection_and_cursor(
                    &query_field,
                    &qbuf,
                    srect.x + 10.0,
                    srect.y + 7.0,
                    18.0,
                    theme,
                    now - state.last_cursor_activity,
                    sctx,
                );
            }
            if !query_field.text.is_empty() {
                self.draw_buffer_runs(&qbuf, srect.x + 10.0, srect.y + 7.0, &[], None, state, sctx);
            }
            y += search_h;
        }

        // 3. List Viewport (Dropdown Drawer OR Virtualized Palette Rows)
        let list_h = (composer_top - y - 4.0).max(ROW_HEIGHT);
        let list_rect = LogicalRect::new(card_x + 10.0, y, card_w - 20.0, list_h);
        state.palette_viewport_rect = Some(list_rect);

        if let Some(dd) = state.palette.dropdown {
            let opts = state.palette.build_dropdown_options(dd, &state.config);
            let sel = state.palette.dropdown_selected.min(opts.len().saturating_sub(1));
            let scroll = state
                .palette
                .dropdown_scroll
                .min(opts.len().saturating_sub(MAX_VISIBLE_ROWS));

            // Gliding highlight pill
            if sel >= scroll && sel < scroll + MAX_VISIBLE_ROWS && !opts.is_empty() {
                let hy = y + state.palette_dropdown_highlight_y.value;
                self.fill_rrect_logical(
                    list_rect.x,
                    hy,
                    list_rect.w,
                    ROW_HEIGHT,
                    8.0,
                    theme.surface_hover.with_alpha(ctx.alpha_mul),
                    true,
                );
            }

            for (vis_idx, (abs_idx, opt)) in opts
                .iter()
                .enumerate()
                .skip(scroll)
                .take(MAX_VISIBLE_ROWS)
                .enumerate()
            {
                let row_y = y + (vis_idx as f32) * ROW_STEP;
                let row_rect = LogicalRect::new(list_rect.x, row_y, list_rect.w, ROW_HEIGHT);
                state.hit_zones.push(HitZone {
                    rect: row_rect,
                    target: HitTarget::PaletteDropdownOption(abs_idx),
                });
                let hov = matches!(
                    state.hovered_hit(),
                    Some(HitTarget::PaletteDropdownOption(i)) if i == abs_idx
                );
                if hov && abs_idx != sel {
                    self.fill_rrect_logical(
                        row_rect.x,
                        row_rect.y,
                        row_rect.w,
                        row_rect.h,
                        8.0,
                        theme.surface.with_alpha(ctx.alpha_mul * 0.7),
                        true,
                    );
                }

                let (tbuf, _) = self.shape_plain_field(
                    &opt.title,
                    13.0,
                    18.0,
                    220.0,
                    if opt.is_active { theme.accent } else { theme.fg },
                    false,
                    Wrap::None,
                );
                self.draw_buffer_runs(&tbuf, row_rect.x + 12.0, row_rect.y + 7.0, &[], None, state, ctx);

                let (sbuf, _) = self.shape_plain_field(
                    &opt.subtitle,
                    12.0,
                    18.0,
                    row_rect.w - 280.0,
                    theme.muted,
                    false,
                    Wrap::None,
                );
                self.draw_buffer_runs(&sbuf, row_rect.x + 195.0, row_rect.y + 7.0, &[], None, state, ctx);

                if opt.is_active {
                    let (abuf, _) =
                        self.shape_plain_field("Active", 11.2, 16.0, 60.0, theme.accent, false, Wrap::None);
                    let aw = measure_buffer_width(&abuf);
                    self.draw_buffer_runs(
                        &abuf,
                        row_rect.x + row_rect.w - aw - 12.0,
                        row_rect.y + 8.0,
                        &[],
                        None,
                        state,
                        ctx,
                    );
                }
            }
            self.draw_palette_scrollbar(opts.len(), scroll, list_rect, theme, ctx);
            return;
        }

        let rows = state.palette_rows();
        if rows.is_empty() {
            let empty_msg = state
                .palette
                .dir_error
                .clone()
                .unwrap_or_else(|| "No matching items".to_string());
            let (ebuf, _) = self.shape_plain_field(
                &empty_msg,
                12.5,
                18.0,
                list_rect.w - 24.0,
                theme.muted,
                false,
                Wrap::None,
            );
            self.draw_buffer_runs(&ebuf, list_rect.x + 12.0, y + 7.0, &[], None, state, ctx);
            return;
        }

        let sel = state
            .palette
            .current_frame()
            .map(|f| f.selected.min(rows.len().saturating_sub(1)))
            .unwrap_or(0);
        let scroll = state
            .palette
            .current_frame()
            .map(|f| f.scroll_row.min(rows.len().saturating_sub(MAX_VISIBLE_ROWS)))
            .unwrap_or(0);

        if sel >= scroll && sel < scroll + MAX_VISIBLE_ROWS {
            let hy = y + state.palette_highlight_y.value;
            self.fill_rrect_logical(
                list_rect.x,
                hy,
                list_rect.w,
                ROW_HEIGHT,
                8.0,
                theme.surface_hover.with_alpha(ctx.alpha_mul),
                true,
            );
        }

        for (vis_idx, (abs_idx, row)) in rows
            .iter()
            .enumerate()
            .skip(scroll)
            .take(MAX_VISIBLE_ROWS)
            .enumerate()
        {
            let row_y = y + (vis_idx as f32) * ROW_STEP;
            let row_rect = LogicalRect::new(list_rect.x, row_y, list_rect.w, ROW_HEIGHT);
            state.hit_zones.push(HitZone {
                rect: row_rect,
                target: HitTarget::PaletteRow(abs_idx),
            });

            let hov = matches!(state.hovered_hit(), Some(HitTarget::PaletteRow(i)) if i == abs_idx);
            if hov && abs_idx != sel {
                self.fill_rrect_logical(
                    row_rect.x,
                    row_rect.y,
                    row_rect.w,
                    row_rect.h,
                    8.0,
                    theme.surface.with_alpha(ctx.alpha_mul * 0.65),
                    true,
                );
            }

            let title_mono = row.title.starts_with('/');
            let title_col = if row.is_active { theme.accent } else { theme.fg };
            let (tbuf, _) = self.shape_plain_field(
                &row.title,
                13.0,
                18.0,
                210.0,
                title_col,
                title_mono,
                Wrap::None,
            );
            let title_w = measure_buffer_width(&tbuf).min(195.0);
            self.draw_buffer_runs(&tbuf, row_rect.x + 12.0, row_rect.y + 7.0, &[], None, state, ctx);

            // Inline editing TextField inside Provider submenu
            if let Some(field_kind) = row.editable {
                if state.palette.editing_field == Some(field_kind) {
                    let ed_x = row_rect.x + 132.0;
                    let ed_w = (row_rect.w - 144.0).max(140.0);
                    let ed_rect = LogicalRect::new(ed_x, row_rect.y + 2.0, ed_w, ROW_HEIGHT - 4.0);
                    self.fill_rrect_logical(
                        ed_rect.x,
                        ed_rect.y,
                        ed_rect.w,
                        ed_rect.h,
                        6.0,
                        theme.bg.with_alpha(ctx.alpha_mul),
                        true,
                    );
                    self.stroke_rrect_logical(
                        ed_rect.x,
                        ed_rect.y,
                        ed_rect.w,
                        ed_rect.h,
                        6.0,
                        1.0,
                        focus_border.with_alpha(ctx.alpha_mul),
                    );
                    let display = if row.masked && !state.palette.edit_buffer.text.is_empty() {
                        mask_field(&state.palette.edit_buffer)
                    } else {
                        state.palette.edit_buffer.clone()
                    };
                    let (ebuf, _) = self.shape_plain_field(
                        &display.text,
                        12.5,
                        17.0,
                        ed_w - 16.0,
                        theme.fg,
                        true,
                        Wrap::None,
                    );
                    let eclip = ClipBox {
                        x0: ((ed_rect.x + 6.0) * ctx.scale).floor() as i32,
                        y0: ((ed_rect.y + 2.0) * ctx.scale).floor() as i32,
                        x1: ((ed_rect.x + ed_rect.w - 6.0) * ctx.scale).ceil() as i32,
                        y1: ((ed_rect.y + ed_rect.h - 2.0) * ctx.scale).ceil() as i32,
                    }
                    .intersect(ctx.clip);
                    let ectx = DrawContext { clip: eclip, ..ctx };
                    let caret_x = ebuf
                        .layout_runs()
                        .next()
                        .map(|run| glyph_x_for_byte(run.glyphs, display.cursor))
                        .unwrap_or(0.0);
                    let offset = (caret_x - (ed_w - 20.0)).max(0.0);
                    let tx = ed_rect.x + 8.0 - offset;
                    if state.has_keyboard_focus {
                        self.draw_field_selection_and_cursor(
                            &display,
                            &ebuf,
                            tx,
                            ed_rect.y + 5.5,
                            17.0,
                            theme,
                            now - state.last_cursor_activity,
                            ectx,
                        );
                    }
                    if !display.text.is_empty() {
                        self.draw_buffer_runs(&ebuf, tx, ed_rect.y + 5.5, &[], None, state, ectx);
                    }
                    continue;
                }
            }

            let mut right_used = 12.0;
            if row.is_submenu {
                let (abuf, _) =
                    self.shape_plain_field("▸", 12.0, 16.0, 20.0, theme.muted, false, Wrap::None);
                self.draw_buffer_runs(
                    &abuf,
                    row_rect.x + row_rect.w - 18.0,
                    row_rect.y + 8.0,
                    &[],
                    None,
                    state,
                    ctx,
                );
                right_used = 24.0;
            }
            if let Some(badge) = &row.value_badge {
                let (bbuf, _) = self.shape_plain_field(
                    badge,
                    11.2,
                    15.0,
                    190.0,
                    if row.is_active { theme.accent } else { theme.fg },
                    false,
                    Wrap::None,
                );
                let bw = (measure_buffer_width(&bbuf) + 14.0).min(185.0);
                let bx = row_rect.x + row_rect.w - right_used - bw;
                let brect = LogicalRect::new(bx, row_rect.y + 5.0, bw, 22.0);
                self.fill_rrect_logical(
                    brect.x,
                    brect.y,
                    brect.w,
                    brect.h,
                    6.0,
                    theme.surface.with_alpha(ctx.alpha_mul),
                    true,
                );
                if row.is_dropdown {
                    self.stroke_rrect_logical(
                        brect.x,
                        brect.y,
                        brect.w,
                        brect.h,
                        6.0,
                        1.0,
                        theme.line.with_alpha(ctx.alpha_mul),
                    );
                }
                self.draw_buffer_runs(&bbuf, brect.x + 7.0, brect.y + 3.5, &[], None, state, ctx);
                right_used += bw + 10.0;
            }

            if let Some(sub) = &row.subtitle {
                let sub_txt = if row.masked {
                    "•".repeat(sub.chars().count().min(20))
                } else {
                    sub.clone()
                };
                let sub_x = (row_rect.x + title_w + 26.0).max(row_rect.x + 132.0);
                let sub_max_w = (row_rect.x + row_rect.w - right_used - sub_x).max(40.0);
                if sub_max_w > 48.0 {
                    let (sbuf, _) = self.shape_plain_field(
                        &sub_txt,
                        12.0,
                        18.0,
                        sub_max_w,
                        theme.muted,
                        row.editable.is_some(),
                        Wrap::None,
                    );
                    let sctx = DrawContext {
                        clip: ctx.clip.intersect(ClipBox {
                            x0: (sub_x * ctx.scale).floor() as i32,
                            x1: ((sub_x + sub_max_w) * ctx.scale).floor() as i32,
                            ..ctx.clip
                        }),
                        ..ctx
                    };
                    self.draw_buffer_runs(&sbuf, sub_x, row_rect.y + 7.0, &[], None, state, sctx);
                }
            }
        }

        self.draw_palette_scrollbar(rows.len(), scroll, list_rect, theme, ctx);
    }

    fn draw_palette_scrollbar(
        &mut self,
        total_rows: usize,
        scroll_row: usize,
        list_rect: LogicalRect,
        theme: Theme,
        ctx: DrawContext,
    ) {
        if total_rows <= MAX_VISIBLE_ROWS {
            return;
        }
        let max_s = (total_rows - MAX_VISIBLE_ROWS) as f32;
        let track_h = (MAX_VISIBLE_ROWS as f32) * ROW_STEP - 8.0;
        let thumb_h = ((MAX_VISIBLE_ROWS as f32 / total_rows as f32) * track_h).clamp(18.0, track_h);
        let frac = (scroll_row as f32 / max_s).clamp(0.0, 1.0);
        let ty = list_rect.y + 2.0 + frac * (track_h - thumb_h);
        self.fill_rrect_logical(
            list_rect.x + list_rect.w + 2.0,
            ty,
            3.0,
            thumb_h,
            1.5,
            theme.faint.with_alpha(ctx.alpha_mul * 0.75),
            true,
        );
    }

    fn estimate_settings_height(&self, state: &AppState) -> f32 {
        if !state.discovered_models.is_empty() || state.models_status.is_some() {
            344.0
        } else {
            306.0
        }
    }

    fn draw_settings_panel(
        &mut self,
        state: &mut AppState,
        card_x: f32,
        card_y: f32,
        card_w: f32,
        above_h: f32,
        ctx: DrawContext,
    ) {
        let theme = state.theme;
        let pad_x = card_x + 20.0;
        let inner_w = card_w - 40.0;
        let mut y = card_y + 16.0;
        let now = state.now_secs();
        let focus_border = Rgba::lerp_premultiplied(theme.line, theme.accent, 0.65);
        let kb = state.has_keyboard_focus;

        let (title_buf, _) =
            self.shape_plain_field("Settings", 14.0, 20.0, 200.0, theme.fg, false, Wrap::None);
        self.draw_buffer_runs(&title_buf, pad_x, y + 2.0, &[], None, state, ctx);

        // Done pill
        let done_rect = LogicalRect::new(card_x + card_w - 72.0, y - 1.0, 52.0, 25.0);
        state.hit_zones.push(HitZone {
            rect: done_rect,
            target: HitTarget::SettingsClose,
        });
        let done_hov = matches!(state.hovered_hit(), Some(HitTarget::SettingsClose));
        let done_kb = kb && state.focus_target == FocusTarget::SettingsDone;
        self.fill_rrect_logical(
            done_rect.x,
            done_rect.y,
            done_rect.w,
            done_rect.h,
            7.0,
            theme
                .accent
                .with_alpha(ctx.alpha_mul * if done_hov { 0.88 } else { 1.0 }),
            true,
        );
        if done_kb {
            self.stroke_rrect_logical(
                done_rect.x - 1.5,
                done_rect.y - 1.5,
                done_rect.w + 3.0,
                done_rect.h + 3.0,
                8.0,
                1.2,
                theme.fg.with_alpha(ctx.alpha_mul),
            );
        }
        let (done_buf, _) = self.shape_plain_field(
            "Done",
            12.0,
            16.0,
            50.0,
            Self::accent_ink(&theme),
            false,
            Wrap::None,
        );
        self.draw_buffer_runs(
            &done_buf,
            done_rect.x + 11.0,
            done_rect.y + 4.5,
            &[],
            None,
            state,
            ctx,
        );

        // Dark / Light mode pills
        for (idx, (mode, label)) in [(ThemeMode::Dark, "Dark"), (ThemeMode::Light, "Light")]
            .iter()
            .enumerate()
        {
            let tw = 46.0;
            let tx = done_rect.x - 104.0 + (idx as f32) * 49.0;
            let trect = LogicalRect::new(tx, y - 1.0, tw, 25.0);
            state.hit_zones.push(HitZone {
                rect: trect,
                target: HitTarget::ThemeToggle(*mode),
            });
            let active = state.config.theme == *mode;
            let kb_foc = kb && state.focus_target == FocusTarget::SettingsTheme(*mode);
            if active || kb_foc {
                self.fill_rrect_logical(
                    trect.x,
                    trect.y,
                    trect.w,
                    trect.h,
                    7.0,
                    theme.surface_hover.with_alpha(ctx.alpha_mul),
                    true,
                );
                self.stroke_rrect_logical(
                    trect.x,
                    trect.y,
                    trect.w,
                    trect.h,
                    7.0,
                    1.0,
                    if kb_foc { theme.accent } else { focus_border }.with_alpha(ctx.alpha_mul),
                );
            }
            let (tbuf, _) = self.shape_plain_field(
                label,
                11.8,
                16.0,
                tw,
                if active { theme.fg } else { theme.muted },
                false,
                Wrap::None,
            );
            self.draw_buffer_runs(&tbuf, trect.x + 9.0, trect.y + 4.5, &[], None, state, ctx);
        }

        y += 32.0;

        // Provider Profile dropdown + Protocol + Vision + New/Delete Custom pills
        let mut px = pad_x;
        let prof_label = format!("Profile: {}  ▾", state.config.active_profile_label());
        let (pbuf, _) =
            self.shape_plain_field(&prof_label, 11.5, 16.0, 210.0, theme.fg, false, Wrap::None);
        let pw = (measure_buffer_width(&pbuf) + 18.0).clamp(96.0, 210.0);
        let prect = LogicalRect::new(px, y, pw, 25.0);
        state.hit_zones.push(HitZone {
            rect: prect,
            target: HitTarget::SettingsProfileToggle,
        });
        let prof_foc = kb && state.focus_target == FocusTarget::SettingsProfile;
        self.fill_rrect_logical(
            prect.x,
            prect.y,
            prect.w,
            prect.h,
            7.0,
            if state.settings_profile_dropdown {
                theme.surface_hover
            } else {
                theme.surface
            }
            .with_alpha(ctx.alpha_mul),
            true,
        );
        self.stroke_rrect_logical(
            prect.x,
            prect.y,
            prect.w,
            prect.h,
            7.0,
            1.0,
            if prof_foc || state.settings_profile_dropdown {
                focus_border
            } else {
                theme.line
            }
            .with_alpha(ctx.alpha_mul),
        );
        self.draw_buffer_runs(
            &pbuf,
            prect.x + 9.0,
            prect.y + 4.5,
            &[],
            None,
            state,
            DrawContext {
                clip: ctx.clip.intersect(ClipBox {
                    x0: (prect.x * ctx.scale).ceil() as i32,
                    x1: ((prect.x + prect.w - 6.0) * ctx.scale).floor() as i32,
                    ..ctx.clip
                }),
                ..ctx
            },
        );
        px += pw + 6.0;

        let proto_lbl = format!("API: {}", state.config.active_format().label());
        let proto_foc = kb && state.focus_target == FocusTarget::SettingsProtocol;
        px += self.draw_settings_pill(
            state,
            &proto_lbl,
            px,
            y,
            HitTarget::SettingsProtocolToggle,
            false,
            proto_foc,
            ctx,
        ) + 6.0;

        let vis_lbl = if state.config.vision {
            "Vision: On"
        } else {
            "Vision: Off"
        };
        let vis_foc = kb && state.focus_target == FocusTarget::SettingsVision;
        px += self.draw_settings_pill(
            state,
            vis_lbl,
            px,
            y,
            HitTarget::SettingsVisionToggle,
            state.config.vision,
            vis_foc,
            ctx,
        ) + 6.0;

        let new_foc = kb && state.focus_target == FocusTarget::SettingsNewProfile;
        px += self.draw_settings_pill(
            state,
            "+ Custom",
            px,
            y,
            HitTarget::SettingsNewProfile,
            false,
            new_foc,
            ctx,
        ) + 6.0;

        if state.config.active_profile_is_custom() && px + 56.0 <= card_x + card_w - 20.0 {
            let del_foc = kb && state.focus_target == FocusTarget::SettingsDeleteProfile;
            self.draw_settings_pill(
                state,
                "Del",
                px,
                y,
                HitTarget::SettingsDeleteProfile,
                false,
                del_foc,
                ctx,
            );
        }

        let dropdown_anchor_y = y + 29.0;
        y += 34.0;

        // Base URL
        self.draw_labeled_settings_field(
            state,
            "Base URL",
            FocusTarget::SettingsBaseUrl,
            false,
            None,
            pad_x,
            y,
            inner_w,
            now,
            ctx,
        );
        y += 60.0;

        // Optional / keyless API Key
        let key_action = if state.field_api_key.text.is_empty() {
            Some((
                "Keyless",
                HitTarget::ToggleKeyMask,
                false,
                false,
            ))
        } else if state.show_api_key {
            Some((
                "Hide",
                HitTarget::ToggleKeyMask,
                true,
                kb && state.focus_target == FocusTarget::SettingsKeyMask,
            ))
        } else {
            Some((
                "Show",
                HitTarget::ToggleKeyMask,
                true,
                kb && state.focus_target == FocusTarget::SettingsKeyMask,
            ))
        };
        self.draw_labeled_settings_field(
            state,
            "API Key  (optional — leave empty for keyless local endpoints)",
            FocusTarget::SettingsApiKey,
            !state.show_api_key,
            key_action,
            pad_x,
            y,
            inner_w,
            now,
            ctx,
        );
        y += 60.0;

        // Model
        self.draw_labeled_settings_field(
            state,
            "Model",
            FocusTarget::SettingsModel,
            false,
            Some((
                if state.loading_models { "..." } else { "Detect" },
                HitTarget::RefreshModels,
                true,
                kb && state.focus_target == FocusTarget::SettingsRefresh,
            )),
            pad_x,
            y,
            inner_w,
            now,
            ctx,
        );
        y += 62.0;

        if y + 24.0 <= card_y + above_h {
            if !state.discovered_models.is_empty() {
                let models_snapshot: Vec<String> =
                    state.discovered_models.iter().take(6).cloned().collect();
                let mut mx = pad_x;
                for (idx, model_id) in models_snapshot.iter().enumerate() {
                    let (mbuf, _) = self.shape_plain_field(
                        model_id,
                        11.5,
                        16.0,
                        220.0,
                        theme.fg,
                        true,
                        Wrap::None,
                    );
                    let mw = (measure_buffer_width(&mbuf) + 16.0).clamp(48.0, 210.0);
                    if mx + mw > card_x + card_w - 20.0 {
                        break;
                    }
                    let mrect = LogicalRect::new(mx, y, mw, 23.0);
                    state.hit_zones.push(HitZone {
                        rect: mrect,
                        target: HitTarget::ModelChip(idx),
                    });
                    let selected = state.field_model.text.trim() == model_id.as_str();
                    let chip_kb = kb && state.focus_target == FocusTarget::SettingsModelChip(idx);
                    self.fill_rrect_logical(
                        mrect.x,
                        mrect.y,
                        mrect.w,
                        mrect.h,
                        6.0,
                        if selected || chip_kb {
                            theme.surface_hover
                        } else {
                            theme.surface
                        }
                        .with_alpha(ctx.alpha_mul),
                        true,
                    );
                    if selected || chip_kb {
                        self.stroke_rrect_logical(
                            mrect.x,
                            mrect.y,
                            mrect.w,
                            mrect.h,
                            6.0,
                            1.0,
                            if chip_kb { theme.accent } else { focus_border }.with_alpha(ctx.alpha_mul),
                        );
                    }
                    self.draw_buffer_runs(
                        &mbuf,
                        mrect.x + 8.0,
                        mrect.y + 3.5,
                        &[],
                        None,
                        state,
                        DrawContext {
                            clip: ClipBox {
                                x0: ctx.clip.x0.max(((mrect.x + 8.0) * ctx.scale).ceil() as i32),
                                x1: ctx.clip.x1.min(((mrect.x + mrect.w - 8.0) * ctx.scale).floor() as i32),
                                ..ctx.clip
                            },
                            ..ctx
                        },
                    );
                    mx += mw + 6.0;
                }
            } else if let Some(status) = state.models_status.clone() {
                let (sbuf, _) = self.shape_plain_field(
                    &status,
                    11.5,
                    16.0,
                    inner_w,
                    theme.muted,
                    false,
                    Wrap::None,
                );
                self.draw_buffer_runs(&sbuf, pad_x, y + 2.0, &[], None, state, ctx);
            }
        }

        // Floating Profile Dropdown overlay when open in Settings panel
        if state.settings_profile_dropdown {
            let profiles: Vec<_> = state.config.all_profiles().to_vec();
            let dd_w = 320.0f32.min(inner_w);
            let dd_h = ((profiles.len().min(7) as f32) * 30.0 + 10.0).min(above_h - 60.0);
            let dd_rect = LogicalRect::new(pad_x, dropdown_anchor_y, dd_w, dd_h);
            self.fill_rrect_logical(
                dd_rect.x,
                dd_rect.y,
                dd_rect.w,
                dd_rect.h,
                10.0,
                theme.float.with_alpha(ctx.alpha_mul),
                true,
            );
            self.stroke_rrect_logical(
                dd_rect.x,
                dd_rect.y,
                dd_rect.w,
                dd_rect.h,
                10.0,
                1.0,
                focus_border.with_alpha(ctx.alpha_mul),
            );
            for (idx, prof) in profiles.iter().enumerate().take(7) {
                let ry = dd_rect.y + 5.0 + (idx as f32) * 30.0;
                let rrect = LogicalRect::new(dd_rect.x + 5.0, ry, dd_rect.w - 10.0, 26.0);
                state.hit_zones.push(HitZone {
                    rect: rrect,
                    target: HitTarget::SettingsProfileOption(idx),
                });
                let is_sel = idx == state.settings_profile_selected;
                let is_hov = matches!(
                    state.hovered_hit(),
                    Some(HitTarget::SettingsProfileOption(i)) if i == idx
                );
                if is_sel || is_hov {
                    self.fill_rrect_logical(
                        rrect.x,
                        rrect.y,
                        rrect.w,
                        rrect.h,
                        6.0,
                        theme.surface_hover.with_alpha(ctx.alpha_mul),
                        true,
                    );
                }
                let is_act = prof.id == state.config.active_profile_id;
                let (nbuf, _) = self.shape_plain_field(
                    &prof.name,
                    12.0,
                    16.0,
                    140.0,
                    if is_act { theme.accent } else { theme.fg },
                    false,
                    Wrap::None,
                );
                self.draw_buffer_runs(&nbuf, rrect.x + 8.0, rrect.y + 5.0, &[], None, state, ctx);
                let (fbuf, _) = self.shape_plain_field(
                    prof.format.label(),
                    11.0,
                    16.0,
                    90.0,
                    theme.muted,
                    false,
                    Wrap::None,
                );
                let fw = measure_buffer_width(&fbuf);
                self.draw_buffer_runs(
                    &fbuf,
                    rrect.x + rrect.w - fw - 10.0,
                    rrect.y + 5.5,
                    &[],
                    None,
                    state,
                    ctx,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_settings_pill(
        &mut self,
        state: &mut AppState,
        label: &str,
        x: f32,
        y: f32,
        target: HitTarget,
        active: bool,
        kb_focused: bool,
        ctx: DrawContext,
    ) -> f32 {
        let theme = state.theme;
        let focus_border = Rgba::lerp_premultiplied(theme.line, theme.accent, 0.65);
        let (buf, _) =
            self.shape_plain_field(label, 11.5, 16.0, 140.0, theme.fg, false, Wrap::None);
        let pw = (measure_buffer_width(&buf) + 16.0).max(40.0);
        let rect = LogicalRect::new(x, y, pw, 25.0);
        state.hit_zones.push(HitZone { rect, target });
        let hov = state.hovered_hit() == Some(target);
        self.fill_rrect_logical(
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            7.0,
            if active || hov || kb_focused {
                theme.surface_hover
            } else {
                theme.surface
            }
            .with_alpha(ctx.alpha_mul),
            true,
        );
        if active || kb_focused {
            self.stroke_rrect_logical(
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                7.0,
                1.0,
                if kb_focused { theme.accent } else { focus_border }.with_alpha(ctx.alpha_mul),
            );
        }
        self.draw_buffer_runs(&buf, rect.x + 8.0, rect.y + 4.5, &[], None, state, ctx);
        pw
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_labeled_settings_field(
        &mut self,
        state: &mut AppState,
        label: &str,
        target: FocusTarget,
        masked: bool,
        right_pill: Option<(&str, HitTarget, bool, bool)>,
        x: f32,
        y: f32,
        w: f32,
        now: f64,
        ctx: DrawContext,
    ) {
        let theme = state.theme;
        let focus_border = Rgba::lerp_premultiplied(theme.line, theme.accent, 0.65);

        let (lbuf, _) =
            self.shape_plain_field(label, 11.5, 15.0, w, theme.muted, false, Wrap::None);
        self.draw_buffer_runs(&lbuf, x + 2.0, y, &[], None, state, ctx);

        let box_rect = LogicalRect::new(x, y + 18.0, w, 34.0);
        state.hit_zones.push(HitZone {
            rect: box_rect,
            target: HitTarget::Input(target),
        });

        let focused = state.has_keyboard_focus && state.focus_target == target;
        self.fill_rrect_logical(
            box_rect.x,
            box_rect.y,
            box_rect.w,
            box_rect.h,
            8.0,
            theme.surface.with_alpha(ctx.alpha_mul),
            true,
        );
        self.stroke_rrect_logical(
            box_rect.x,
            box_rect.y,
            box_rect.w,
            box_rect.h,
            8.0,
            1.0,
            if focused { focus_border } else { theme.line }.with_alpha(ctx.alpha_mul),
        );

        let mut right_reserve = 12.0;
        if let Some((pill_txt, pill_hit, interactive, pill_kb)) = right_pill {
            let (pbuf, _) = self.shape_plain_field(
                pill_txt,
                11.0,
                15.0,
                90.0,
                if interactive { theme.fg } else { theme.faint },
                false,
                Wrap::None,
            );
            let pw = (measure_buffer_width(&pbuf) + 14.0).max(44.0);
            let prect =
                LogicalRect::new(box_rect.x + box_rect.w - pw - 6.0, box_rect.y + 5.0, pw, 24.0);
            if interactive {
                state.hit_zones.push(HitZone {
                    rect: prect,
                    target: pill_hit,
                });
                self.fill_rrect_logical(
                    prect.x,
                    prect.y,
                    prect.w,
                    prect.h,
                    6.0,
                    theme.surface_hover.with_alpha(ctx.alpha_mul),
                    true,
                );
                if pill_kb {
                    self.stroke_rrect_logical(
                        prect.x,
                        prect.y,
                        prect.w,
                        prect.h,
                        6.0,
                        1.0,
                        theme.accent.with_alpha(ctx.alpha_mul),
                    );
                }
            }
            self.draw_buffer_runs(&pbuf, prect.x + 7.0, prect.y + 4.5, &[], None, state, ctx);
            right_reserve = pw + 14.0;
        }

        let field = match target {
            FocusTarget::SettingsBaseUrl => &state.field_base_url,
            FocusTarget::SettingsApiKey => &state.field_api_key,
            FocusTarget::SettingsModel => &state.field_model,
            _ => &state.composer,
        };

        let display_field: TextField = if masked && !field.text.is_empty() {
            mask_field(field)
        } else {
            field.clone()
        };

        let (fbuf, _) = self.shape_plain_field(
            &display_field.text,
            13.0,
            19.0,
            (w - 22.0 - right_reserve).max(60.0),
            theme.fg,
            true,
            Wrap::None,
        );

        let field_clip = ClipBox {
            x0: ((box_rect.x + 8.0) * self.scale).floor() as i32,
            y0: ((box_rect.y + 3.0) * self.scale).floor() as i32,
            x1: ((box_rect.x + box_rect.w - right_reserve) * self.scale).ceil() as i32,
            y1: ((box_rect.y + box_rect.h - 3.0) * self.scale).ceil() as i32,
        }
        .intersect(ctx.clip);

        let fctx = DrawContext {
            scale: self.scale,
            clip: field_clip,
            alpha_mul: ctx.alpha_mul,
        };

        let caret_x = fbuf.layout_runs().next().map(|run|
            glyph_x_for_byte(run.glyphs, display_field.cursor)).unwrap_or(0.0);
        let text_room = (box_rect.w - 22.0 - right_reserve).max(20.0);
        let offset = if focused { (caret_x - text_room + 3.0).max(0.0) } else { 0.0 };
        let tx = box_rect.x + 11.0 - offset;
        let ty = box_rect.y + 7.5;
        if focused {
            self.draw_field_selection_and_cursor(
                &display_field,
                &fbuf,
                tx,
                ty,
                19.0,
                theme,
                now - state.last_cursor_activity,
                fctx,
            );
        }
        if !display_field.text.is_empty() {
            self.draw_buffer_runs(&fbuf, tx, ty, &[], None, state, fctx);
        }
    }

    fn cached_message(&mut self, msg: &ChatItem, msg_idx: usize, inner_w: f32,
        now: f64, theme: &Theme, copied: Option<(usize, usize, f64)>) -> Rc<ShapedMessage> {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        msg_idx.hash(&mut hash);
        inner_w.to_bits().hash(&mut hash);
        [theme.fg, theme.muted, theme.faint, theme.accent, theme.success, theme.danger,
            theme.syn_keyword, theme.syn_string, theme.syn_number, theme.syn_type, theme.syn_comment]
            .hash(&mut hash);
        (msg.role == Role::User).hash(&mut hash);
        if msg.role == Role::User {
            msg.raw_content.hash(&mut hash);
            msg.attachments.len().hash(&mut hash);
            for a in &msg.attachments {
                a.name.hash(&mut hash);
                a.size_bytes.hash(&mut hash);
            }
        } else {
            msg.stream.shown_text().hash(&mut hash);
        }
        msg.stream.reasoning.hash(&mut hash);
        msg.stream.reasoning_active.hash(&mut hash);
        msg.stream.reasoning_duration_secs.map(f64::to_bits).hash(&mut hash);
        msg.stream.is_live().hash(&mut hash);
        msg.streaming.hash(&mut hash);
        msg.thinking_expanded.hash(&mut hash);
        msg.error.hash(&mut hash);
        copied.map(|(m,b,expiry)| (m,b,now < expiry)).hash(&mut hash);
        for word in msg.stream.frontier().iter().chain(&msg.stream.reasoning_frontier) {
            word.start.hash(&mut hash); word.end.hash(&mut hash); word.birth_secs.to_bits().hash(&mut hash);
        }
        let key = hash.finish();
        if let Some((_, _, scene)) = self.message_cache.iter().find(|(index, k, _)| *index == msg_idx && *k == key) {
            return Rc::clone(scene);
        }
        let scene = Rc::new(self.shape_message(msg, msg_idx, inner_w, now, theme, copied));
        if let Some(entry) = self.message_cache.iter_mut().find(|(index, _, _)| *index == msg_idx) {
            *entry = (msg_idx, key, Rc::clone(&scene));
        } else {
            if self.message_cache.len() >= 128 { self.message_cache.remove(0); }
            self.message_cache.push((msg_idx, key, Rc::clone(&scene)));
        }
        scene
    }

    fn shape_message(
        &mut self,
        msg: &ChatItem,
        msg_idx: usize,
        inner_w: f32,
        now: f64,
        theme: &Theme,
        copied_badge: Option<(usize, usize, f64)>,
    ) -> ShapedMessage {

        if msg.role == Role::User {
            let max_bubble_w = (inner_w * 0.78).max(160.0);
            let mut user_chips = Vec::new();
            let mut chips_w_needed = 0.0f32;
            let mut chips_h = 0.0f32;
            if !msg.attachments.is_empty() {
                let mut rx = 0.0f32;
                let mut rows = 1usize;
                for att in &msg.attachments {
                    let tag = att.kind.badge();
                    let short = truncate_label(&att.name, 18);
                    let lbl = if let Some(pages) = att.page_count {
                        format!("{}  {} · {}p", tag, short, pages)
                    } else {
                        format!("{}  {} · {}", tag, short, format_bytes(att.size_bytes))
                    };
                    let (cbuf, _) = self.shape_plain_field(
                        &lbl,
                        11.0,
                        15.0,
                        max_bubble_w - 28.0,
                        theme.muted,
                        false,
                        Wrap::None,
                    );
                    let cw = (measure_buffer_width(&cbuf) + 16.0).min(max_bubble_w - 28.0);
                    if rx > 0.0 && rx + cw > max_bubble_w - 28.0 {
                        rows += 1;
                        rx = 0.0;
                    }
                    chips_w_needed = chips_w_needed.max(rx + cw);
                    rx += cw + 6.0;
                    user_chips.push((lbl, cw));
                }
                chips_h = (rows as f32) * 22.0 + ((rows.saturating_sub(1)) as f32) * 5.0 + 8.0;
            }
            let (buf, text_h) = self.shape_plain_field(
                &msg.raw_content,
                14.5,
                22.0,
                max_bubble_w - 28.0,
                theme.fg,
                false,
                Wrap::WordOrGlyph,
            );
            let text_w = measure_buffer_width(&buf)
                .max(chips_w_needed)
                .min(max_bubble_w - 28.0);
            let bubble_w = (text_w + 28.0).clamp(48.0, max_bubble_w);
            let bubble_h = (text_h + 18.0 + chips_h).max(38.0);
            return ShapedMessage {
                msg_idx,
                role: Role::User,
                user_buf: Some((buf, bubble_w, bubble_h)),
                user_chips,
                thinking_pill: None,
                blocks: Vec::new(),
                waiting_ttft: false,
                error_buf: None,
                total_height: bubble_h,
            };
        }

        let mut y_h = 0.0f32;

        // Reasoning `<think>` block pill if present on SmoothStream
        let thinking_pill = if let Some(think_text) = msg.stream.reasoning.as_deref() {
            let label = if msg.stream.reasoning_active {
                "Reasoning...".to_string()
            } else if let Some(secs) = msg.stream.reasoning_duration_secs {
                if msg.thinking_expanded {
                    format!("Thought for {:.1}s  ▾", secs)
                } else {
                    format!("Thought for {:.1}s  ▸", secs)
                }
            } else if msg.thinking_expanded {
                "Thought  ▾".to_string()
            } else {
                "Thought  ▸".to_string()
            };
            let (pbuf, _) =
                self.shape_plain_field(&label, 12.0, 17.0, 240.0, theme.muted, false, Wrap::None);
            let expanded_body = if !think_text.trim().is_empty() {
                let spans = reasoning_spans(think_text, &msg.stream.reasoning_frontier, theme.muted);
                let (tbuf, th) = self.shape_rich_spans(
                    &spans,
                    13.0,
                    20.0,
                    inner_w - 18.0,
                    theme.muted,
                );
                y_h += th + 10.0;
                Some((tbuf, th))
            } else {
                None
            };
            y_h += 28.0;
            Some((pbuf, expanded_body))
        } else {
            None
        };

        let visible_md = msg.stream.shown_text();
        let ParsedMarkdown {
            blocks: md_blocks,
            links: _,
            frontier_states: _,
        } = parse_markdown(
            visible_md,
            msg.stream.is_live(),
            msg.stream.frontier(),
            now,
            theme,
        );

        let mut shaped_blocks: Vec<ShapedBlock> = Vec::new();
        for (b_idx, block) in md_blocks.iter().enumerate() {
            let is_first = b_idx == 0 && thinking_pill.is_none();
            match block {
                MdBlock::Heading { level, spans } => {
                    let (fsize, lheight, gap) = match level {
                        1 => (20.5, 28.0, if is_first { 0.0 } else { 14.0 }),
                        2 => (17.0, 24.5, if is_first { 0.0 } else { 12.0 }),
                        _ => (15.2, 22.5, if is_first { 0.0 } else { 10.0 }),
                    };
                    let (buf, h) = self.shape_rich_spans(spans, fsize, lheight, inner_w, theme.fg);
                    y_h += gap + h;
                    shaped_blocks.push(ShapedBlock::Text {
                        buffer: buf,
                        height: h,
                        indent_x: 0.0,
                        quote_bar: false,
                        marker: None,
                        top_gap: gap,
                        msg_idx,
                    });
                }
                MdBlock::Paragraph { spans } => {
                    let gap = if is_first { 0.0 } else { 9.0 };
                    let (buf, h) = self.shape_rich_spans(spans, 14.5, 23.0, inner_w, theme.fg);
                    y_h += gap + h;
                    shaped_blocks.push(ShapedBlock::Text {
                        buffer: buf,
                        height: h,
                        indent_x: 0.0,
                        quote_bar: false,
                        marker: None,
                        top_gap: gap,
                        msg_idx,
                    });
                }
                MdBlock::ListItem {
                    depth,
                    marker,
                    spans,
                } => {
                    let gap = if is_first { 0.0 } else { 5.0 };
                    let indent = 22.0 + (*depth as f32) * 18.0;
                    let (buf, h) = self.shape_rich_spans(
                        spans,
                        14.5,
                        22.5,
                        (inner_w - indent).max(100.0),
                        theme.fg,
                    );
                    y_h += gap + h;
                    shaped_blocks.push(ShapedBlock::Text {
                        buffer: buf,
                        height: h,
                        indent_x: indent,
                        quote_bar: false,
                        marker: Some(*marker),
                        top_gap: gap,
                        msg_idx,
                    });
                }
                MdBlock::BlockQuote { spans } => {
                    let gap = if is_first { 0.0 } else { 10.0 };
                    let (buf, h) =
                        self.shape_rich_spans(spans, 14.2, 22.0, inner_w - 18.0, theme.muted);
                    y_h += gap + h + 4.0;
                    shaped_blocks.push(ShapedBlock::Text {
                        buffer: buf,
                        height: h + 4.0,
                        indent_x: 16.0,
                        quote_bar: true,
                        marker: None,
                        top_gap: gap,
                        msg_idx,
                    });
                }
                MdBlock::CodeBlock { lang, spans, .. } => {
                    let gap = if is_first { 0.0 } else { 11.0 };
                    let header_h = 28.0;
                    let lang_buf = if !lang.is_empty() {
                        Some(
                            self.shape_plain_field(
                                lang,
                                11.2,
                                15.0,
                                180.0,
                                theme.muted,
                                true,
                                Wrap::None,
                            )
                            .0,
                        )
                    } else {
                        None
                    };
                    let is_copied = matches!(
                        copied_badge,
                        Some((m, b, exp)) if m == msg_idx && b == b_idx && now < exp
                    );
                    let copy_label = if is_copied { "Copied" } else { "Copy" };
                    let (copy_buf, _) = self.shape_plain_field(
                        copy_label,
                        11.2,
                        15.0,
                        80.0,
                        if is_copied { theme.success } else { theme.muted },
                        false,
                        Wrap::None,
                    );
                    let (code_buf, ch) =
                        self.shape_rich_spans_wrapped(spans, 12.8, 19.5, inner_w - 26.0, theme.fg, Wrap::None);
                    let card_code_h = ch.max(19.5) + 20.0;
                    y_h += gap + header_h + card_code_h;
                    shaped_blocks.push(ShapedBlock::Code {
                        lang_buf,
                        copy_buf,
                        code_buf,
                        code_h: card_code_h,
                        header_h,
                        top_gap: gap,
                        msg_idx,
                        block_idx: b_idx,
                        copied: is_copied,
                    });
                }
                MdBlock::Table { headers, rows } => {
                    let gap = if is_first { 0.0 } else { 11.0 };
                    let cols = headers
                        .len()
                        .max(rows.iter().map(|r| r.len()).max().unwrap_or(1))
                        .max(1);
                    let col_widths: Vec<f32> = (0..cols).map(|c| {
                        let natural = headers.get(c).into_iter().chain(rows.iter().filter_map(|r| r.get(c)))
                            .map(|spans| spans.iter().map(|s| s.text.chars().count()).sum::<usize>())
                            .max().unwrap_or(0) as f32 * 7.5 + 24.0;
                        (inner_w / cols as f32).max(natural.clamp(100.0, 420.0))
                    }).collect();

                    let mut header_cells = Vec::with_capacity(cols);
                    let mut hh = 20.0f32;
                    for c in 0..cols {
                        let empty: Vec<RichSpan> = Vec::new();
                        let sp = headers.get(c).unwrap_or(&empty);
                        let (cbuf, ch) =
                            self.shape_rich_spans(sp, 13.0, 19.0, col_widths[c] - 16.0, theme.fg);
                        hh = hh.max(ch);
                        header_cells.push(cbuf);
                    }
                    let header_h = hh + 12.0;

                    let mut body_rows = Vec::with_capacity(rows.len());
                    let mut body_total = 0.0f32;
                    for row in rows {
                        let mut rbufs = Vec::with_capacity(cols);
                        let mut rh = 19.0f32;
                        for c in 0..cols {
                            let empty: Vec<RichSpan> = Vec::new();
                            let sp = row.get(c).unwrap_or(&empty);
                            let (cbuf, ch) =
                                self.shape_rich_spans(sp, 13.0, 19.0, col_widths[c] - 16.0, theme.fg);
                            rh = rh.max(ch);
                            rbufs.push(cbuf);
                        }
                        let row_h = rh + 11.0;
                        body_total += row_h;
                        body_rows.push((rbufs, row_h));
                    }
                    y_h += gap + header_h + body_total;
                    shaped_blocks.push(ShapedBlock::Table {
                        col_widths,
                        header_cells,
                        header_h,
                        body_rows,
                        top_gap: gap,
                        msg_idx,
                        block_idx: b_idx,
                    });
                }
                MdBlock::Rule => {
                    let gap = if is_first { 4.0 } else { 12.0 };
                    y_h += gap + 9.0;
                    shaped_blocks.push(ShapedBlock::Rule { top_gap: gap });
                }
            }
        }

        let waiting_ttft = msg.streaming
            && shaped_blocks.is_empty()
            && thinking_pill.is_none()
            && msg.error.is_none();
        if waiting_ttft {
            y_h += 24.0;
        }

        let error_buf = if let Some(err) = &msg.error {
            let (ebuf, eh) = self.shape_plain_field(
                err,
                13.0,
                19.5,
                inner_w - 94.0,
                theme.danger,
                false,
                Wrap::WordOrGlyph,
            );
            let box_h = (eh + 18.0).max(36.0);
            y_h += if y_h > 0.0 { 10.0 + box_h } else { box_h };
            Some((ebuf, box_h))
        } else {
            None
        };

        ShapedMessage {
            msg_idx,
            role: Role::Assistant,
            user_buf: None,
            user_chips: Vec::new(),
            thinking_pill,
            blocks: shaped_blocks,
            waiting_ttft,
            error_buf,
            total_height: y_h.max(22.0),
        }
    }

    /// Shared horizontal viewport used by tables and code. Horizontal touchpad
    /// gestures and Shift+wheel scroll only the hovered component.
    fn overflow_viewport(&mut self, state: &mut AppState, key: (usize, usize), rect: LogicalRect,
        content_w: f32, ctx: DrawContext) -> (f32, DrawContext) {
        let max_x = (content_w - rect.w).max(0.0);
        let offset = state.block_scroll.entry(key).or_default();
        *offset = (*offset).clamp(0.0, max_x);
        let value = *offset;
        let clip = ctx.clip.intersect(ClipBox {
            x0: (rect.x * ctx.scale).ceil() as i32, y0: (rect.y * ctx.scale).ceil() as i32,
            x1: ((rect.x + rect.w) * ctx.scale).floor() as i32,
            y1: ((rect.y + rect.h) * ctx.scale).floor() as i32,
        });
        if max_x > 0.5 && clip.x1 > clip.x0 && clip.y1 > clip.y0 {
            state.scroll_regions.push(crate::state::ScrollRegion {
                key, max_x, rect: LogicalRect::new(clip.x0 as f32 / ctx.scale, clip.y0 as f32 / ctx.scale,
                    (clip.x1 - clip.x0) as f32 / ctx.scale, (clip.y1 - clip.y0) as f32 / ctx.scale),
            });
            // Sleek 3px scrollbar track and thumb
            let track_x = rect.x;
            let track_w = rect.w;
            self.fill_rrect_logical(track_x, rect.y + rect.h - 4.0, track_w, 3.0, 1.5,
                state.theme.line.with_alpha(ctx.alpha_mul * 0.30), true);
            let thumb_w = (track_w * track_w / content_w).max(28.0).min(track_w);
            let thumb_x = track_x + (track_w - thumb_w) * (value / max_x);
            self.fill_rrect_logical(thumb_x, rect.y + rect.h - 4.0,
                thumb_w, 3.0, 1.5, state.theme.muted.with_alpha(ctx.alpha_mul * 0.65), true);
        }
        (value, DrawContext { clip, ..ctx })
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_messages_viewport(
        &mut self,
        state: &mut AppState,
        shaped_msgs: &[Rc<ShapedMessage>],
        card_x: f32,
        card_y: f32,
        card_w: f32,
        viewport_h: f32,
        scroll_y: f32,
        ctx: DrawContext,
    ) {
        let theme = state.theme;
        let code_bg = Self::code_card_bg(&theme);
        let pad_x = card_x + 22.0;
        let inner_w = card_w - 44.0;
        let now = state.now_secs();

        let mut cur_y = card_y + 18.0 - scroll_y;

        for sm in shaped_msgs {
            let message_h = sm.height(state);
            let entrance = state.messages[sm.msg_idx].entrance.value.clamp(0.0, 1.0);
            let frontier_states: Vec<_> = state.messages[sm.msg_idx].stream.frontier().iter()
                .filter_map(|word| word.evaluate(now)).collect();
            let msg_bottom = cur_y + message_h;
            if msg_bottom < card_y - 24.0 || cur_y > card_y + viewport_h + 24.0 {
                cur_y += message_h + 18.0;
                continue;
            }

            let slide_y = (1.0 - entrance) * 8.0;
            let mut my = cur_y + slide_y;
            let mctx = DrawContext {
                scale: ctx.scale,
                clip: ctx.clip,
                alpha_mul: ctx.alpha_mul * entrance,
            };

            if sm.role == Role::User {
                if let Some((buf, bw, bh)) = &sm.user_buf {
                    let bx = pad_x + inner_w - *bw;
                    self.fill_rrect_logical(
                        bx,
                        my,
                        *bw,
                        *bh,
                        15.0,
                        theme.bubble.with_alpha(mctx.alpha_mul),
                        true,
                    );
                    self.stroke_rrect_logical(
                        bx,
                        my,
                        *bw,
                        *bh,
                        15.0,
                        1.0,
                        theme.line.with_alpha(mctx.alpha_mul),
                    );
                    let mut text_off_y = my + 9.0;
                    if !sm.user_chips.is_empty() {
                        let start_cx = bx + 14.0;
                        let max_cx = bx + *bw - 14.0;
                        let mut cx = start_cx;
                        let mut cy = my + 9.0;
                        for (lbl, cw) in &sm.user_chips {
                            if cx > start_cx && cx + *cw > max_cx {
                                cx = start_cx;
                                cy += 27.0;
                            }
                            self.fill_rrect_logical(
                                cx,
                                cy,
                                *cw,
                                22.0,
                                6.0,
                                theme.surface.with_alpha(mctx.alpha_mul),
                                true,
                            );
                            let (cbuf, _) = self.shape_plain_field(
                                lbl,
                                11.0,
                                15.0,
                                *cw - 12.0,
                                theme.muted,
                                false,
                                Wrap::None,
                            );
                            self.draw_buffer_runs(&cbuf, cx + 8.0, cy + 3.5, &[], None, state, mctx);
                            cx += *cw + 6.0;
                        }
                        text_off_y = cy + 29.0;
                    }
                    self.draw_buffer_runs(buf, bx + 14.0, text_off_y, &[], None, state, mctx);
                }
                cur_y += message_h + 18.0;
                continue;
            }

            if let Some((pbuf, expanded_opt)) = &sm.thinking_pill {
                let pw = (measure_buffer_width(pbuf) + 18.0).max(84.0);
                let prect = LogicalRect::new(pad_x, my, pw, 23.0);
                if prect.y + prect.h > card_y && prect.y < card_y + viewport_h {
                    state.hit_zones.push(HitZone {
                        rect: prect,
                        target: HitTarget::ToggleThinking(sm.msg_idx),
                    });
                }
                self.fill_rrect_logical(
                    prect.x,
                    prect.y,
                    prect.w,
                    prect.h,
                    7.0,
                    theme.surface.with_alpha(mctx.alpha_mul),
                    true,
                );
                self.draw_buffer_runs(pbuf, prect.x + 9.0, prect.y + 3.0, &[], None, state, mctx);
                my += 28.0;

                if let Some((tbuf, th)) = expanded_opt {
                    let visible_h = (*th + 10.0) * state.messages[sm.msg_idx].thinking_reveal.value.clamp(0.0, 1.0);
                    let line_h = th.min(visible_h).min((card_y + viewport_h - my).max(0.0));
                    if line_h > 0.0 {
                        self.fill_rect_logical(
                            pad_x + 3.0,
                            my,
                            2.0,
                            line_h,
                            theme.accent.with_alpha(mctx.alpha_mul * 0.40),
                            true,
                        );
                    }
                    let reasoning_states: Vec<_> = state.messages[sm.msg_idx].stream.reasoning_frontier.iter().map(|word| {
                        let t = ((now - word.birth_secs) / 0.28).clamp(0.0, 1.0) as f32;
                        let ease = t * (2.0 - t);
                        let sigma = 1.1 * (1.0 - ease);
                        let opacity = 0.25 + 0.75 * ease;
                        let offset_y = 0.8 * (1.0 - ease);
                        WordFocusState { sigma_logical: sigma, opacity, offset_y }
                    }).collect();
                    let body_clip_y1 = ((my + visible_h) * ctx.scale).floor() as i32;
                    let viewport_bottom_px = ((card_y + viewport_h) * ctx.scale).floor() as i32;
                    let body_ctx = DrawContext { clip: mctx.clip.intersect(ClipBox {
                        x0: ((pad_x + 14.0) * ctx.scale).floor() as i32,
                        y0: (my * ctx.scale).floor() as i32,
                        x1: ((pad_x + inner_w) * ctx.scale).ceil() as i32,
                        y1: body_clip_y1.min(viewport_bottom_px),
                    }), ..mctx };
                    self.draw_buffer_runs(tbuf, pad_x + 14.0, my, &reasoning_states, None, state, body_ctx);
                    my += visible_h;
                }
            }

            for block in &sm.blocks {
                match block {
                    ShapedBlock::Text {
                        buffer,
                        height,
                        indent_x,
                        quote_bar,
                        marker,
                        top_gap,
                        msg_idx,
                    } => {
                        my += *top_gap;
                        if *quote_bar {
                            self.fill_rrect_logical(
                                pad_x,
                                my + 2.0,
                                2.5,
                                (*height - 4.0).max(8.0),
                                1.2,
                                theme.accent.with_alpha(mctx.alpha_mul * 0.65),
                                true,
                            );
                        }
                        if let Some(mk) = marker {
                            let mx = pad_x + (*indent_x - 16.0).max(0.0);
                            match mk {
                                ListMarker::Bullet => {
                                    self.fill_rrect_logical(
                                        mx + 2.0,
                                        my + 9.0,
                                        4.5,
                                        4.5,
                                        2.25,
                                        theme.muted.with_alpha(mctx.alpha_mul),
                                        true,
                                    );
                                }
                                ListMarker::Ordered(n) => {
                                    let lbl = format!("{}.", n);
                                    let (nbuf, _) = self.shape_plain_field(
                                        &lbl,
                                        13.0,
                                        22.5,
                                        28.0,
                                        theme.muted,
                                        false,
                                        Wrap::None,
                                    );
                                    self.draw_buffer_runs(
                                        &nbuf,
                                        mx - 4.0,
                                        my,
                                        &[],
                                        None,
                                        state,
                                        mctx,
                                    );
                                }
                                ListMarker::Task(done) => {
                                    let cbx = mx - 1.0;
                                    let cby = my + 4.5;
                                    if *done {
                                        self.fill_rrect_logical(
                                            cbx,
                                            cby,
                                            13.0,
                                            13.0,
                                            3.5,
                                            theme.accent.with_alpha(mctx.alpha_mul),
                                            true,
                                        );
                                        self.draw_checkmark(
                                            cbx + 3.0,
                                            cby + 6.5,
                                            Self::accent_ink(&theme).with_alpha(mctx.alpha_mul),
                                        );
                                    } else {
                                        self.stroke_rrect_logical(
                                            cbx,
                                            cby,
                                            13.0,
                                            13.0,
                                            3.5,
                                            1.2,
                                            theme.muted.with_alpha(mctx.alpha_mul),
                                        );
                                    }
                                }
                            }
                        }

                        self.draw_buffer_runs(
                            buffer,
                            pad_x + *indent_x,
                            my,
                            &frontier_states,
                            Some(*msg_idx),
                            state,
                            mctx,
                        );
                        my += *height;
                    }
                    ShapedBlock::Code {
                        lang_buf,
                        copy_buf,
                        code_buf,
                        code_h,
                        header_h,
                        top_gap,
                        msg_idx,
                        block_idx,
                        copied,
                    } => {
                        my += *top_gap;
                        let total_ch = *header_h + *code_h;
                        self.fill_rrect_logical(
                            pad_x,
                            my,
                            inner_w,
                            total_ch,
                            11.0,
                            code_bg.with_alpha(mctx.alpha_mul),
                            true,
                        );
                        self.stroke_rrect_logical(
                            pad_x,
                            my,
                            inner_w,
                            total_ch,
                            11.0,
                            1.0,
                            theme.line.with_alpha(mctx.alpha_mul),
                        );
                        self.fill_rect_logical(
                            pad_x + 1.0,
                            my + *header_h,
                            inner_w - 2.0,
                            1.0,
                            theme.line.with_alpha(mctx.alpha_mul),
                            true,
                        );
                        if let Some(lbuf) = lang_buf {
                            self.draw_buffer_runs(
                                lbuf,
                                pad_x + 12.0,
                                my + 6.5,
                                &[],
                                None,
                                state,
                                cell_context(mctx, pad_x + 8.0, (inner_w - 90.0).max(20.0)),
                            );
                        }

                        let cw = (measure_buffer_width(copy_buf) + 16.0).max(46.0);
                        let crect =
                            LogicalRect::new(pad_x + inner_w - cw - 6.0, my + 3.0, cw, 22.0);
                        if crect.y + crect.h > card_y && crect.y < card_y + viewport_h {
                            state.hit_zones.push(HitZone {
                                rect: crect,
                                target: HitTarget::CopyCode {
                                    msg_idx: *msg_idx,
                                    block_idx: *block_idx,
                                },
                            });
                        }
                        let chov = matches!(
                            state.hovered_hit(),
                            Some(HitTarget::CopyCode { msg_idx: m, block_idx: b })
                                if m == *msg_idx && b == *block_idx
                        );
                        if chov || *copied {
                            self.fill_rrect_logical(
                                crect.x,
                                crect.y,
                                crect.w,
                                crect.h,
                                5.5,
                                theme.surface_hover.with_alpha(mctx.alpha_mul),
                                true,
                            );
                        }
                        self.draw_buffer_runs(
                            copy_buf,
                            crect.x + 8.0,
                            crect.y + 3.5,
                            &[],
                            None,
                            state,
                            mctx,
                        );
                        let (offset, code_ctx) = self.overflow_viewport(state, (*msg_idx, *block_idx),
                            LogicalRect::new(pad_x + 13.0, my + *header_h + 8.0, inner_w - 26.0, *code_h - 12.0),
                            measure_buffer_width(code_buf), mctx);
                        self.draw_buffer_runs(
                            code_buf,
                            pad_x + 13.0 - offset,
                            my + *header_h + 10.0,
                            &frontier_states,
                            None,
                            state,
                            code_ctx,
                        );
                        my += total_ch;
                    }
                    ShapedBlock::Table {
                        col_widths,
                        header_cells,
                        header_h,
                        body_rows,
                        top_gap,
                        msg_idx,
                        block_idx,
                    } => {
                        my += *top_gap;
                        let body_h: f32 = body_rows.iter().map(|(_, h)| *h).sum();
                        let total_th = *header_h + body_h;
                        self.fill_rrect_logical(
                            pad_x,
                            my,
                            inner_w,
                            total_th,
                            9.0,
                            code_bg.with_alpha(mctx.alpha_mul),
                            true,
                        );
                        self.stroke_rrect_logical(
                            pad_x,
                            my,
                            inner_w,
                            total_th,
                            9.0,
                            1.0,
                            theme.line.with_alpha(mctx.alpha_mul),
                        );
                        self.fill_rect_logical(
                            pad_x + 1.0,
                            my + 1.0,
                            inner_w - 2.0,
                            *header_h - 1.0,
                            theme.surface.with_alpha(mctx.alpha_mul),
                            true,
                        );

                        let content_w: f32 = col_widths.iter().sum();
                        let (offset, table_ctx) = self.overflow_viewport(state, (*msg_idx, *block_idx),
                            LogicalRect::new(pad_x + 1.0, my + 1.0, inner_w - 2.0, total_th - 2.0), content_w, mctx);

                        // Subtle vertical column dividers
                        let mut div_x = pad_x - offset;
                        for (c, w) in col_widths.iter().enumerate() {
                            if c > 0 && div_x > pad_x + 2.0 && div_x < pad_x + inner_w - 2.0 {
                                self.fill_rect_logical(
                                    div_x,
                                    my + 1.0,
                                    1.0,
                                    total_th - 2.0,
                                    theme.line.with_alpha(mctx.alpha_mul * 0.32),
                                    true,
                                );
                            }
                            div_x += *w;
                        }

                        let mut cx = pad_x - offset;
                        for (c, cbuf) in header_cells.iter().enumerate() {
                            self.draw_buffer_runs(
                                cbuf,
                                cx + 8.0,
                                my + 6.0,
                                &frontier_states,
                                Some(*msg_idx),
                                state,
                                cell_context(table_ctx, cx, col_widths[c]),
                            );
                            cx += col_widths.get(c).copied().unwrap_or(80.0);
                        }
                        let mut ry = my + *header_h;
                        for (row_bufs, rh) in body_rows {
                            self.fill_rect_logical(
                                pad_x + 1.0,
                                ry,
                                inner_w - 2.0,
                                1.0,
                                theme.line.with_alpha(mctx.alpha_mul * row_focus_opacity(row_bufs, &frontier_states)),
                                true,
                            );
                            let mut rcx = pad_x - offset;
                            for (c, cbuf) in row_bufs.iter().enumerate() {
                                self.draw_buffer_runs(
                                    cbuf,
                                    rcx + 8.0,
                                    ry + 5.5,
                                    &frontier_states,
                                    Some(*msg_idx),
                                    state,
                                    cell_context(table_ctx, rcx, col_widths[c]),
                                );
                                rcx += col_widths.get(c).copied().unwrap_or(80.0);
                            }
                            ry += *rh;
                        }
                        my += total_th;
                    }
                    ShapedBlock::Rule { top_gap } => {
                        my += *top_gap;
                        self.fill_rect_logical(
                            pad_x,
                            my + 4.0,
                            inner_w,
                            1.0,
                            theme.line.with_alpha(mctx.alpha_mul),
                            true,
                        );
                        my += 9.0;
                    }
                }
            }

            if sm.waiting_ttft {
                for i in 0..3 {
                    let phase = (now * 3.8 - (i as f64) * 0.45).sin() as f32 * 0.5 + 0.5;
                    let alpha = (0.22 + 0.65 * phase) * mctx.alpha_mul;
                    self.fill_rrect_logical(
                        pad_x + (i as f32) * 11.0,
                        my + 9.0,
                        5.5,
                        5.5,
                        2.75,
                        theme.muted.with_alpha(alpha),
                        true,
                    );
                }
                my += 24.0;
            }

            if let Some((ebuf, box_h)) = &sm.error_buf {
                if !sm.blocks.is_empty() {
                    my += 10.0;
                }
                self.fill_rrect_logical(
                    pad_x,
                    my,
                    inner_w,
                    *box_h,
                    9.0,
                    theme.danger.with_alpha(mctx.alpha_mul * 0.12),
                    true,
                );
                self.stroke_rrect_logical(
                    pad_x,
                    my,
                    inner_w,
                    *box_h,
                    9.0,
                    1.0,
                    theme.danger.with_alpha(mctx.alpha_mul * 0.42),
                );
                self.draw_buffer_runs(ebuf, pad_x + 12.0, my + 9.0, &[], None, state, mctx);

                let sbtn =
                    LogicalRect::new(pad_x + inner_w - 76.0, my + (*box_h - 24.0) * 0.5, 66.0, 24.0);
                if sbtn.y + sbtn.h > card_y && sbtn.y < card_y + viewport_h {
                    state.hit_zones.push(HitZone {
                        rect: sbtn,
                        target: HitTarget::OpenSettings,
                    });
                }
                self.fill_rrect_logical(
                    sbtn.x,
                    sbtn.y,
                    sbtn.w,
                    sbtn.h,
                    6.0,
                    theme.surface_hover.with_alpha(mctx.alpha_mul),
                    true,
                );
                let (sbuf, _) = self.shape_plain_field(
                    "Settings",
                    11.5,
                    16.0,
                    64.0,
                    theme.fg,
                    false,
                    Wrap::None,
                );
                self.draw_buffer_runs(&sbuf, sbtn.x + 10.0, sbtn.y + 4.0, &[], None, state, mctx);
            }

            cur_y += message_h + 18.0;
        }

        if state.max_scroll > 2.0 {
            let track_h = (viewport_h - 16.0).max(24.0);
            let total_h = viewport_h + state.max_scroll;
            let thumb_h = ((viewport_h / total_h) * track_h).clamp(24.0, track_h);
            let frac = (scroll_y / state.max_scroll).clamp(0.0, 1.0);
            let thumb_y = card_y + 8.0 + frac * (track_h - thumb_h);
            self.fill_rrect_logical(
                card_x + card_w - 6.5,
                thumb_y,
                3.0,
                thumb_h,
                1.5,
                theme.faint.with_alpha(ctx.alpha_mul * 0.75),
                true,
            );
        }
    }

    fn shape_plain_field(
        &mut self,
        text: &str,
        font_size: f32,
        line_height: f32,
        max_w: f32,
        color: Rgba,
        monospace: bool,
        wrap: Wrap,
    ) -> (Buffer, f32) {
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(font_size, line_height));
        buffer.set_wrap(&mut self.font_system, wrap);
        buffer.set_size(&mut self.font_system, Some(max_w.max(20.0)), None);
        let family = if monospace {
            Family::Monospace
        } else {
            Family::SansSerif
        };
        let attrs = Attrs::new().family(family).color(color.to_cosmic());
        buffer.set_text(&mut self.font_system, text, &attrs, Shaping::Advanced);
        buffer.shape_until_scroll(&mut self.font_system, false);
        let lines = buffer.layout_runs().count().max(1);
        (buffer, (lines as f32) * line_height)
    }

    fn shape_rich_spans(&mut self, spans: &[RichSpan], font_size: f32, line_height: f32,
        max_w: f32, color: Rgba) -> (Buffer, f32) {
        self.shape_rich_spans_wrapped(spans, font_size, line_height, max_w, color, Wrap::WordOrGlyph)
    }

    fn shape_rich_spans_wrapped(
        &mut self,
        spans: &[RichSpan],
        font_size: f32,
        line_height: f32,
        max_w: f32,
        default_color: Rgba,
        wrap: Wrap,
    ) -> (Buffer, f32) {
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(font_size, line_height));
        buffer.set_wrap(&mut self.font_system, wrap);
        let buffer_w = if wrap == Wrap::None { None } else { Some(max_w.max(20.0)) };
        buffer.set_size(&mut self.font_system, buffer_w, None);
        let base_attrs = Attrs::new()
            .family(Family::SansSerif)
            .color(default_color.to_cosmic());
        let iter = spans.iter().map(|s| {
            let mut a = Attrs::new()
                .family(if s.monospace {
                    Family::Monospace
                } else {
                    Family::SansSerif
                })
                .weight(if s.bold {
                    Weight::SEMIBOLD
                } else {
                    Weight::NORMAL
                })
                .style(if s.italic {
                    Style::Italic
                } else {
                    Style::Normal
                })
                .color(CosmicColor::rgba(s.color.r, s.color.g, s.color.b, s.color.a))
                .metadata(s.meta);
            if s.monospace && (s.meta & META_CODESPAN) != 0 {
                a = a.metrics(Metrics::new((font_size * 0.89).max(11.0), line_height));
            }
            (s.text.as_str(), a)
        });
        buffer.set_rich_text(&mut self.font_system, iter, &base_attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        let lines = buffer.layout_runs().count().max(1);
        (buffer, (lines as f32) * line_height)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_buffer_runs(
        &mut self,
        buffer: &Buffer,
        origin_x: f32,
        origin_y: f32,
        frontier_states: &[WordFocusState],
        msg_idx_opt: Option<usize>,
        state: &mut AppState,
        ctx: DrawContext,
    ) {
        let decorated = buffer.layout_runs().any(|run| run.glyphs.iter().any(|g| g.metadata & 0xFF != 0));
        let saved_mask = if decorated {
            let saved = self.mask.clone();
            self.restrict_mask(ctx.clip);
            Some(saved)
        } else { None };
        let scale = ctx.scale;
        let theme = state.theme;
        let pw = self.pixmap.width();
        let ph = self.pixmap.height();
        let line_h = buffer.metrics().line_height;

        for run in buffer.layout_runs() {
            let glyphs = run.glyphs;
            if glyphs.is_empty() {
                continue;
            }
            let run_top = origin_y + run.line_top;
            if (run_top + line_h + 12.0) * scale < ctx.clip.y0 as f32
                || (run_top - 12.0) * scale > ctx.clip.y1 as f32
            {
                continue;
            }

            // Pass 1: Inline backgrounds & link underlines
            let mut gi = 0usize;
            while gi < glyphs.len() {
                let meta = glyphs[gi].metadata;
                let flags = meta & 0xFF;
                if flags != 0 {
                    let start_x = origin_x + glyphs[gi].x;
                    let mut end_x = origin_x + glyphs[gi].x + glyphs[gi].w;
                    let mut gj = gi + 1;
                    while gj < glyphs.len() && glyphs[gj].metadata == meta {
                        end_x = origin_x + glyphs[gj].x + glyphs[gj].w;
                        gj += 1;
                    }

                    let focus_alpha = unpack_frontier_idx(meta)
                        .and_then(|fi| frontier_states.get(fi))
                        .map(|fs| fs.opacity)
                        .unwrap_or(1.0);
                    let focus_dy = unpack_frontier_idx(meta)
                        .and_then(|fi| frontier_states.get(fi))
                        .map(|fs| fs.offset_y)
                        .unwrap_or(0.0);

                    let span_w = (end_x - start_x).max(2.0);
                    let pill_y = run_top + 2.0 + focus_dy;
                    let pill_h = (line_h - 4.0).max(13.0);

                    if (flags & META_MARK) != 0 {
                        self.fill_rrect_logical(
                            start_x - 1.5,
                            pill_y,
                            span_w + 3.0,
                            pill_h,
                            4.0,
                            theme.mark.with_alpha(ctx.alpha_mul * focus_alpha),
                            true,
                        );
                    }
                    if (flags & META_CODESPAN) != 0 {
                        self.fill_rrect_logical(
                            start_x - 1.0,
                            pill_y,
                            span_w + 2.0,
                            pill_h,
                            5.0,
                            theme.surface.with_alpha(ctx.alpha_mul * focus_alpha),
                            true,
                        );
                        self.stroke_rrect_logical(
                            start_x - 1.0,
                            pill_y,
                            span_w + 2.0,
                            pill_h,
                            5.0,
                            1.0,
                            theme.line.with_alpha(ctx.alpha_mul * focus_alpha),
                        );
                    }
                    if (flags & META_LINK) != 0 {
                        self.fill_rect_logical(
                            start_x,
                            origin_y + run.line_y + 2.0 + focus_dy,
                            span_w,
                            1.0,
                            theme.accent.with_alpha(ctx.alpha_mul * focus_alpha * 0.7),
                            true,
                        );
                        if let (Some(msg_idx), Some(link_idx)) =
                            (msg_idx_opt, unpack_link_idx(meta))
                        {
                            let x0 = start_x.max(ctx.clip.x0 as f32 / scale);
                            let y0 = pill_y.max(ctx.clip.y0 as f32 / scale);
                            let x1 = (start_x + span_w).min(ctx.clip.x1 as f32 / scale);
                            let y1 = (pill_y + pill_h).min(ctx.clip.y1 as f32 / scale);
                            if x1 > x0 && y1 > y0 {
                                state.hit_zones.push(HitZone {
                                    rect: LogicalRect::new(x0, y0, x1 - x0, y1 - y0),
                                    target: HitTarget::LinkUrl { msg_idx, link_idx },
                                });
                            }
                        }
                    }
                    gi = gj;
                } else {
                    gi += 1;
                }
            }

            // Pass 2: Settled crisp glyphs vs continuous-sigma Gaussian frontier words
            let mut idx = 0usize;
            while idx < glyphs.len() {
                let g = &glyphs[idx];
                let f_opt = unpack_frontier_idx(g.metadata)
                    .and_then(|fi| frontier_states.get(fi).copied());

                if let Some(focus) = f_opt.filter(|fs| fs.sigma_logical >= 0.25) {
                    let target_meta_f = unpack_frontier_idx(g.metadata);
                    let group_start = idx;
                    idx += 1;
                    while idx < glyphs.len()
                        && unpack_frontier_idx(glyphs[idx].metadata) == target_meta_f
                        && glyphs[idx].color_opt == g.color_opt
                    {
                        idx += 1;
                    }
                    let group = &glyphs[group_start..idx];
                    self.composite_blurred_glyph_group(
                        group,
                        origin_x,
                        origin_y + run.line_y + focus.offset_y,
                        focus.sigma_logical * scale,
                        focus.opacity * ctx.alpha_mul,
                        ctx.clip,
                    );
                } else {
                    let opacity = f_opt.map(|fs| fs.opacity).unwrap_or(1.0) * ctx.alpha_mul;
                    let dy = f_opt.map(|fs| fs.offset_y).unwrap_or(0.0);
                    self.blit_single_glyph(
                        g,
                        origin_x,
                        origin_y + run.line_y + dy,
                        opacity,
                        pw,
                        ph,
                        ctx.clip,
                    );
                    idx += 1;
                }
            }

            // Pass 3: Strikethrough line
            let mut si = 0usize;
            while si < glyphs.len() {
                if (glyphs[si].metadata & META_STRIKE) != 0 {
                    let sx = origin_x + glyphs[si].x;
                    let mut ex = sx + glyphs[si].w;
                    let mut sj = si + 1;
                    while sj < glyphs.len() && (glyphs[sj].metadata & META_STRIKE) != 0 {
                        ex = origin_x + glyphs[sj].x + glyphs[sj].w;
                        sj += 1;
                    }
                    self.fill_rect_logical(
                        sx,
                        origin_y + run.line_y - buffer.metrics().font_size * 0.30,
                        (ex - sx).max(2.0),
                        1.1,
                        theme.muted.with_alpha(ctx.alpha_mul),
                        true,
                    );
                    si = sj;
                } else {
                    si += 1;
                }
            }
        }
        if let Some(mask) = saved_mask { self.mask = mask; }
    }

    #[allow(clippy::too_many_arguments)]
    fn blit_single_glyph(
        &mut self,
        g: &cosmic_text::LayoutGlyph,
        origin_x: f32,
        baseline_y: f32,
        alpha_mul: f32,
        pw: u32,
        ph: u32,
        clip: ClipBox,
    ) {
        let phys = g.physical((origin_x * self.scale, baseline_y * self.scale), self.scale);
        let Some(img) = self
            .swash_cache
            .get_image(&mut self.font_system, phys.cache_key)
            .as_ref()
        else {
            return;
        };
        if img.placement.width == 0 || img.placement.height == 0 {
            return;
        }

        let gx = phys.x + img.placement.left;
        let gy = phys.y - img.placement.top;
        let gw = img.placement.width as i32;
        let gh = img.placement.height as i32;

        let base_c = g
            .color_opt
            .map(|c| Rgba::new(c.r(), c.g(), c.b(), c.a()))
            .unwrap_or(Rgba::new(230, 225, 216, 255))
            .with_alpha(alpha_mul);
        if base_c.a == 0 {
            return;
        }

        let x0 = gx.max(clip.x0).max(0);
        let y0 = gy.max(clip.y0).max(0);
        let x1 = (gx + gw).min(clip.x1).min(pw as i32);
        let y1 = (gy + gh).min(clip.y1).min(ph as i32);
        if x1 <= x0 || y1 <= y0 {
            return;
        }

        let dst = self.pixmap.data_mut();
        let mask_data = self.mask.data();

        match img.content {
            SwashContent::Mask => {
                for py in y0..y1 {
                    let sy = (py - gy) as usize;
                    let srow = sy * (gw as usize);
                    let drow = (py as usize) * (pw as usize);
                    for px in x0..x1 {
                        let m_cov = mask_data[drow + px as usize];
                        if m_cov == 0 {
                            continue;
                        }
                        let cov = img.data[srow + (px - gx) as usize];
                        if cov == 0 {
                            continue;
                        }
                        let eff_cov = ((cov as u32 * m_cov as u32 + 127) / 255) as u8;
                        blend_coverage_pixel(&mut dst[(drow + px as usize) * 4..], base_c, eff_cov);
                    }
                }
            }
            SwashContent::Color | SwashContent::SubpixelMask => {
                for py in y0..y1 {
                    let sy = (py - gy) as usize;
                    let srow = sy * (gw as usize);
                    let drow = (py as usize) * (pw as usize);
                    for px in x0..x1 {
                        let m_cov = mask_data[drow + px as usize];
                        if m_cov == 0 {
                            continue;
                        }
                        let si = (srow + (px - gx) as usize) * 4;
                        let sa = ((img.data[si + 3] as f32) * alpha_mul * (m_cov as f32 / 255.0))
                            .round() as u8;
                        if sa == 0 {
                            continue;
                        }
                        let c = Rgba::new(img.data[si], img.data[si + 1], img.data[si + 2], sa);
                        blend_coverage_pixel(&mut dst[(drow + px as usize) * 4..], c, 255);
                    }
                }
            }
        }
    }

    fn composite_blurred_glyph_group(
        &mut self,
        group: &[cosmic_text::LayoutGlyph],
        origin_x: f32,
        baseline_y: f32,
        sigma_px: f32,
        opacity: f32,
        clip: ClipBox,
    ) {
        if group.is_empty() || opacity <= 0.004 {
            return;
        }
        let base_c = group[0]
            .color_opt
            .map(|c| Rgba::new(c.r(), c.g(), c.b(), c.a()))
            .unwrap_or(Rgba::new(230, 225, 216, 255));

        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        self.glyph_placements.clear();

        for g in group {
            let phys = g.physical((origin_x * self.scale, baseline_y * self.scale), self.scale);
            if let Some(img) = self
                .swash_cache
                .get_image(&mut self.font_system, phys.cache_key)
                .as_ref()
            {
                if img.placement.width > 0 && img.placement.height > 0 {
                    let gx = phys.x + img.placement.left;
                    let gy = phys.y - img.placement.top;
                    let gw = img.placement.width;
                    let gh = img.placement.height;
                    min_x = min_x.min(gx);
                    min_y = min_y.min(gy);
                    max_x = max_x.max(gx + gw as i32);
                    max_y = max_y.max(gy + gh as i32);
                    self.glyph_placements.push((phys.cache_key, gx, gy, gw, gh));
                }
            }
        }

        if self.glyph_placements.is_empty() || max_x <= min_x || max_y <= min_y {
            return;
        }

        let word_w = (max_x - min_x) as usize;
        let word_h = (max_y - min_y) as usize;
        if word_w > 2048 || word_h > 512 {
            // Long unbroken code/identifiers must not vanish just because the
            // optical scratch budget is exceeded. Keep the gentle opacity pull
            // with clipped crisp glyphs instead of allocating an enormous blur.
            let pw = self.pixmap.width();
            let ph = self.pixmap.height();
            for glyph in group {
                self.blit_single_glyph(glyph, origin_x, baseline_y, opacity, pw, ph, clip);
            }
            return;
        }

        let pad = ((sigma_px * 2.65).ceil() as usize).clamp(2, 48);
        let bw = word_w + pad * 2;
        let bh = word_h + pad * 2;

        {
            let src_mask = self.blur_scratch.prepare_mask(bw, bh);
            for (ck, gx, gy, gw, gh) in self.glyph_placements.iter().copied() {
                if let Some(img) = self
                    .swash_cache
                    .get_image(&mut self.font_system, ck)
                    .as_ref()
                {
                    let ox = pad + (gx - min_x) as usize;
                    let oy = pad + (gy - min_y) as usize;
                    match img.content {
                        SwashContent::Mask => {
                            for row in 0..(gh as usize) {
                                let srow = row * (gw as usize);
                                let drow = (oy + row) * bw + ox;
                                for col in 0..(gw as usize) {
                                    let v = img.data[srow + col];
                                    if v > 0 {
                                        let slot = &mut src_mask[drow + col];
                                        *slot = (*slot + (v as f32 / 255.0)).min(1.0);
                                    }
                                }
                            }
                        }
                        SwashContent::Color | SwashContent::SubpixelMask => {
                            for row in 0..(gh as usize) {
                                let srow = row * (gw as usize);
                                let drow = (oy + row) * bw + ox;
                                for col in 0..(gw as usize) {
                                    let v = img.data[(srow + col) * 4 + 3];
                                    if v > 0 {
                                        let slot = &mut src_mask[drow + col];
                                        *slot = (*slot + (v as f32 / 255.0)).min(1.0);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut pm_mut = self.pixmap.as_mut();
        self.blur_scratch.composite_blurred_mask(
            bw,
            bh,
            sigma_px,
            base_c,
            opacity,
            min_x - pad as i32,
            min_y - pad as i32,
            clip.as_tuple(),
            &mut pm_mut,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_field_selection_and_cursor(
        &mut self,
        field: &TextField,
        buffer: &Buffer,
        origin_x: f32,
        origin_y: f32,
        line_h: f32,
        theme: Theme,
        idle_secs: f64,
        ctx: DrawContext,
    ) {
        let saved_mask = self.mask.clone();
        self.restrict_mask(ctx.clip);
        let sel = field.selection_range();
        let (cursor_line, cursor_col_bytes) = byte_offset_to_line_col(&field.text, field.cursor);
        let sel_lines = sel.map(|(a, b)| {
            (
                byte_offset_to_line_col(&field.text, a),
                byte_offset_to_line_col(&field.text, b),
            )
        });

        let mut cursor_xy: Option<(f32, f32)> = None;
        let mut saw_any_run = false;

        for run in buffer.layout_runs() {
            saw_any_run = true;
            let ry = origin_y + run.line_top;

            if let Some(((sl, sc), (el, ec))) = sel_lines {
                if run.line_i >= sl && run.line_i <= el {
                    let min_b = if run.line_i == sl { sc } else { 0 };
                    let max_b = if run.line_i == el { ec } else { usize::MAX };
                    let sx = glyph_x_for_byte(run.glyphs, min_b);
                    let ex = if max_b == usize::MAX {
                        run.glyphs
                            .last()
                            .map(|g| g.x + g.w + 4.0)
                            .unwrap_or(sx + 4.0)
                    } else {
                        glyph_x_for_byte(run.glyphs, max_b)
                    };
                    if ex > sx {
                        self.fill_rrect_logical(
                            origin_x + sx,
                            ry + 1.5,
                            (ex - sx).max(2.0),
                            (line_h - 3.0).max(14.0),
                            3.5,
                            theme.accent.with_alpha(ctx.alpha_mul * 0.24),
                            true,
                        );
                    }
                }
            }

            if run.line_i == cursor_line {
                let run_start = run.glyphs.first().map(|g| g.start).unwrap_or(0);
                let run_end = run.glyphs.last().map(|g| g.end).unwrap_or(0);
                if cursor_xy.is_none()
                    || (cursor_col_bytes >= run_start && cursor_col_bytes <= run_end)
                {
                    let cx = origin_x + glyph_x_for_byte(run.glyphs, cursor_col_bytes);
                    cursor_xy = Some((cx, ry));
                }
            }
        }

        let (cx, cy) = cursor_xy.unwrap_or((
            origin_x,
            origin_y
                + if saw_any_run {
                    (cursor_line as f32) * line_h
                } else {
                    0.0
                },
        ));

        let blink = if idle_secs < 0.45 {
            1.0
        } else {
            let phase = ((idle_secs - 0.45) * std::f64::consts::TAU / 1.06).cos() as f32;
            (phase * 0.5 + 0.5).clamp(0.08, 1.0)
        };

        let ch = (line_h - 4.0).max(14.0);
        if (cx * ctx.scale) >= (ctx.clip.x0 as f32) && (cx * ctx.scale) <= (ctx.clip.x1 as f32) {
            self.fill_rrect_logical(
                cx,
                cy + 2.0,
                1.6,
                ch,
                0.8,
                theme.accent.with_alpha(ctx.alpha_mul * blink),
                true,
            );
        }
        self.mask = saved_mask;
    }

    fn fill_rect_logical(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Rgba,
        clipped_to_card: bool,
    ) {
        if w <= 0.0 || h <= 0.0 || color.a == 0 {
            return;
        }
        let s = self.scale;
        if let Some(rect) =
            tiny_skia::Rect::from_xywh(x * s, y * s, (w * s).max(1.0), (h * s).max(1.0))
        {
            let path = PathBuilder::from_rect(rect);
            let mut paint = Paint::default();
            paint.set_color_rgba8(color.r, color.g, color.b, color.a);
            paint.anti_alias = true;
            let mask_ref = if clipped_to_card {
                Some(&self.mask)
            } else {
                None
            };
            self.pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                mask_ref,
            );
        }
    }

    fn fill_rrect_logical(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        color: Rgba,
        clipped_to_card: bool,
    ) {
        if w <= 0.0 || h <= 0.0 || color.a == 0 {
            return;
        }
        let s = self.scale;
        if let Some(path) = build_rrect_path(x * s, y * s, w * s, h * s, r * s) {
            let mut paint = Paint::default();
            paint.set_color_rgba8(color.r, color.g, color.b, color.a);
            paint.anti_alias = true;
            let mask_ref = if clipped_to_card {
                Some(&self.mask)
            } else {
                None
            };
            self.pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                mask_ref,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn stroke_rrect_logical(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        stroke_w: f32,
        color: Rgba,
    ) {
        if w <= 0.0 || h <= 0.0 || color.a == 0 {
            return;
        }
        let s = self.scale;
        let half = (stroke_w * s) * 0.5;
        if let Some(path) = build_rrect_path(
            x * s + half,
            y * s + half,
            (w * s - stroke_w * s).max(1.0),
            (h * s - stroke_w * s).max(1.0),
            (r * s - half).max(0.0),
        ) {
            let mut paint = Paint::default();
            paint.set_color_rgba8(color.r, color.g, color.b, color.a);
            paint.anti_alias = true;
            let stroke = Stroke {
                width: (stroke_w * s).max(1.0),
                ..Stroke::default()
            };
            self.pixmap
                .stroke_path(&path, &paint, &stroke, Transform::identity(), Some(&self.mask));
        }
    }

    fn draw_up_arrow_icon(&mut self, cx: f32, cy: f32, arm: f32, color: Rgba) {
        let s = self.scale;
        let mut pb = PathBuilder::new();
        pb.move_to(cx * s, (cy + arm) * s);
        pb.line_to(cx * s, (cy - arm) * s);
        pb.move_to((cx - arm * 0.82) * s, (cy - arm * 0.16) * s);
        pb.line_to(cx * s, (cy - arm) * s);
        pb.line_to((cx + arm * 0.82) * s, (cy - arm * 0.16) * s);
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color_rgba8(color.r, color.g, color.b, color.a);
            paint.anti_alias = true;
            let stroke = Stroke {
                width: 1.75 * s,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            self.pixmap
                .stroke_path(&path, &paint, &stroke, Transform::identity(), Some(&self.mask));
        }
    }

    fn draw_checkmark(&mut self, x: f32, y: f32, color: Rgba) {
        let s = self.scale;
        let mut pb = PathBuilder::new();
        pb.move_to(x * s, y * s);
        pb.line_to((x + 2.8) * s, (y + 2.8) * s);
        pb.line_to((x + 7.2) * s, (y - 2.6) * s);
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color_rgba8(color.r, color.g, color.b, color.a);
            paint.anti_alias = true;
            let stroke = Stroke {
                width: 1.6 * s,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            self.pixmap
                .stroke_path(&path, &paint, &stroke, Transform::identity(), Some(&self.mask));
        }
    }

    pub fn copy_to_wayland_argb8888(&self, dst: &mut [u8]) {
        let src = self.pixmap.data();
        let len = dst.len().min(src.len());
        let mut i = 0usize;
        while i + 3 < len {
            dst[i] = src[i + 2];
            dst[i + 1] = src[i + 1];
            dst[i + 2] = src[i];
            dst[i + 3] = src[i + 3];
            i += 4;
        }
    }

    /// Reproducible CPU-render benchmark, not a compositor FPS claim. Use a
    /// release build and compare p50/p95 on the same hardware and scale.
    pub fn benchmark_render() -> Result<(), String> {
        for scale in [1.0, 2.0] {
            let mut renderer = Self::new(scale);
            let mut config = AppConfig::default();
            config.model = "benchmark-no-network".into();
            let mut state = AppState::new(config);
            let markdown = stress_markdown();
            for _ in 0..12 {
                state.messages.push(ChatItem::new_user("Markdown stress fixture".into()));
                let mut reply = ChatItem::new_assistant_streaming(0.0);
                reply.stream = crate::stream::SmoothStream::from_completed(&markdown);
                reply.raw_content = markdown.clone();
                reply.streaming = false;
                reply.entrance.snap_to(1.0);
                state.messages.push(reply);
            }
            let mut simulated_time = 0.0;
            for scenario in ["steady", "route-reversal", "scroll", "thinking", "streaming"] {
                let mut samples = Vec::new();
                for frame in 0..96 {
                    simulated_time += 1.0 / 120.0;
                    state.start_instant = std::time::Instant::now()
                        - std::time::Duration::from_secs_f64(simulated_time);
                    match scenario {
                        "route-reversal" => state.composer.set(if frame / 12 % 2 == 0 { "/" } else { "" }),
                        "scroll" => {
                            state.composer.clear();
                            state.scroll_input(0.0, if frame / 24 % 2 == 0 { 24.0 } else { -24.0 }, true, false);
                        }
                        "thinking" => {
                            state.messages.last_mut().unwrap().thinking_expanded = frame / 24 % 2 == 0;
                        }
                        "streaming" => {
                            let reply = state.messages.last_mut().unwrap();
                            if frame == 0 { reply.stream.start_streaming(simulated_time); }
                            let end = markdown.char_indices().nth((frame + 1) * 12)
                                .map(|(i, _)| i).unwrap_or(markdown.len());
                            reply.stream.update_raw(&markdown[..end], true, simulated_time);
                        }
                        _ => {}
                    }
                    let start = std::time::Instant::now();
                    state.tick(1.0 / 120.0);
                    renderer.render(&mut state);
                    if frame >= 24 { samples.push(start.elapsed().as_secs_f64() * 1000.0); }
                }
                samples.sort_by(f64::total_cmp);
                println!("{{\"scenario\":\"{}\",\"scale\":{},\"messages\":{},\"samples\":{},\"p50_ms\":{:.3},\"p95_ms\":{:.3},\"max_ms\":{:.3}}}",
                    scenario, scale, state.messages.len(), samples.len(), samples[samples.len()/2],
                    samples[samples.len()*95/100], samples[samples.len()-1]);
            }
        }
        Ok(())
    }

    pub fn render_showcase_pngs(out_dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
        std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
        let mut renderer = Self::new(2.0);
        let mut written = Vec::new();

        // 1. Minimalist Thin Floating Bar (zero placeholders, zero tooltips)
        {
            let mut cfg = AppConfig::default();
            cfg.model = "preview-no-network".into();
            let mut st = AppState::new(cfg);
            renderer.panel_transition = PanelTransition::new();
            renderer.last_above_rect = None;
            renderer.outgoing_panel = None;
            st.has_keyboard_focus = true;
            for _ in 0..90 {
                st.tick(1.0 / 120.0);
            }
            renderer.render(&mut st);
            let p = out_dir.join("01-thin-bar.png");
            renderer.pixmap.save_png(&p).map_err(|e| e.to_string())?;
            written.push(p);
        }

        // 2. Active Streaming Blur-to-Focus Frontier
        {
            let mut cfg = AppConfig::default();
            cfg.model = "preview-no-network".into();
            let mut st = AppState::new(cfg);
            renderer.panel_transition = PanelTransition::new();
            renderer.last_above_rect = None;
            renderer.outgoing_panel = None;
            st.has_keyboard_focus = false;
            st.messages.push(ChatItem::new_user(
                "Explain why niri layer-shell overlays usually steal focus.".to_string(),
            ));
            let now = st.now_secs();
            let mut reply = ChatItem::new_assistant_streaming(now);
            let full_md = "## Zero-Hijack Layer Surfaces\n\nOn **niri**, mapping a surface with `Exclusive` keyboard interactivity overrides `KeyboardFocus::Layout` unconditionally. By pairing dynamic `wl_region` input masks with `KeyboardInteractivity::None` your underlying browser keeps every keystroke while words condense smoothly out of optical blur into sharp Geist ink.";
            reply.raw_content = full_md.to_string();
            reply.stream.seed_preview_frontier(full_md, now);
            reply.entrance.snap_to(1.0);
            st.messages.push(reply);
            renderer.render(&mut st);
            for _ in 0..120 {
                st.card_rect.advance(1.0 / 120.0);
                st.card_radius.advance(1.0 / 120.0);
                st.presence_spring.advance(1.0 / 120.0);
            }
            renderer.render(&mut st);
            let p = out_dir.join("02-streaming-blur-focus.png");
            renderer.pixmap.save_png(&p).map_err(|e| e.to_string())?;
            written.push(p);
        }

        // 3. Rich Markdown, Code Highlighting, Task Lists & Light Mode
        {
            let mut cfg = AppConfig::default();
            cfg.theme = ThemeMode::Light;
            cfg.model = "preview-no-network".into();
            let mut st = AppState::new(cfg);
            renderer.panel_transition = PanelTransition::new();
            renderer.last_above_rect = None;
            renderer.outgoing_panel = None;
            st.messages.push(ChatItem::new_user(
                "Show a minimal Rust example and checklist.".to_string(),
            ));
            let md = "Here is a ==zero-copy== mask update for `zwlr_layer_surface_v1`:\n\n```rust\n// Restrict pointer hit-testing to exact card bounds\nfn sync_mask(region: &Region, card: LogicalRect) {\n    region.add(card.x as i32, card.y as i32, card.w as i32, card.h as i32);\n}\n```\n\n- [x] Keyless local `baseUrl` support\n- [x] M3 Expressive spatial springs\n- [ ] Optional multi-session history";
            let mut reply = ChatItem::new_assistant_streaming(0.0);
            reply.raw_content = md.to_string();
            reply.stream = crate::stream::SmoothStream::from_completed(md);
            reply.streaming = false;
            reply.entrance.snap_to(1.0);
            st.messages.push(reply);
            renderer.render(&mut st);
            for _ in 0..120 {
                st.tick(1.0 / 120.0);
            }
            renderer.render(&mut st);
            let p = out_dir.join("03-markdown-light-paper.png");
            renderer.pixmap.save_png(&p).map_err(|e| e.to_string())?;
            written.push(p);
        }

        // 4. Keyless-Capable Settings Panel
        {
            let mut cfg = AppConfig::default();
            cfg.model = "qwen2.5-coder:14b".into();
            let mut st = AppState::new(cfg);
            renderer.panel_transition = PanelTransition::new();
            renderer.last_above_rect = None;
            renderer.outgoing_panel = None;
            st.has_keyboard_focus = true;
            st.open_settings();
            st.loading_models = false;
            st.discovered_models = vec![
                "qwen2.5-coder:14b".to_string(),
                "llama3.2:latest".to_string(),
                "deepseek-r1:8b".to_string(),
            ];
            st.field_model.set("qwen2.5-coder:14b");
            renderer.render(&mut st);
            for _ in 0..120 {
                st.card_rect.advance(1.0 / 120.0);
                st.card_radius.advance(1.0 / 120.0);
                st.presence_spring.advance(1.0 / 120.0);
            }
            renderer.render(&mut st);
            let p = out_dir.join("04-settings-keyless.png");
            renderer.pixmap.save_png(&p).map_err(|e| e.to_string())?;
            written.push(p);
        }

        // 5. Wide code/table and reasoning fixture, clipped while scrolled.
        {
            let mut config = AppConfig::default();
            config.model = "preview-no-network".into();
            let mut st = AppState::new(config);
            let markdown = stress_markdown();
            let mut reply = ChatItem::new_assistant_streaming(0.0);
            reply.stream = crate::stream::SmoothStream::from_completed(&markdown);
            reply.raw_content = markdown;
            reply.streaming = false;
            reply.thinking_expanded = true;
            reply.thinking_reveal.snap_to(1.0);
            reply.entrance.snap_to(1.0);
            st.messages.push(reply);
            renderer.panel_transition = PanelTransition::new();
            renderer.last_above_rect = None;
            renderer.outgoing_panel = None;
            renderer.render(&mut st);
            for _ in 0..120 {
                st.tick(1.0 / 120.0);
            }
            st.scroll_spring.snap_to(110.0);
            st.user_scrolled_up = true;
            renderer.render(&mut st);
            let p = out_dir.join("05-wide-markdown-clipping.png");
            renderer.pixmap.save_png(&p).map_err(|e| e.to_string())?;
            written.push(p);
        }

        // 6. Nested Palette: /settings -> /provider
        {
            let mut cfg = AppConfig::default();
            cfg.select_profile("anthropic");
            cfg.model = "claude-sonnet-4-5".into();
            let mut st = AppState::new(cfg);
            renderer.panel_transition = PanelTransition::new();
            renderer.last_above_rect = None;
            renderer.outgoing_panel = None;
            st.has_keyboard_focus = true;
            st.palette.open_root();
            st.palette.push_route(PaletteRoute::Settings);
            st.palette.push_route(PaletteRoute::Provider);
            for _ in 0..120 {
                st.tick(1.0 / 120.0);
            }
            renderer.render(&mut st);
            let p = out_dir.join("06-palette-settings-provider.png");
            renderer.pixmap.save_png(&p).map_err(|e| e.to_string())?;
            written.push(p);
        }

        // 7. Nested Palette: /attach with composer attachment chips
        {
            let mut cfg = AppConfig::default();
            cfg.model = "preview-no-network".into();
            let mut st = AppState::new(cfg);
            renderer.panel_transition = PanelTransition::new();
            renderer.last_above_rect = None;
            renderer.outgoing_panel = None;
            st.has_keyboard_focus = true;
            st.composer.set("Compare the table on page 2 with this mockup");
            st.attach_raw_image(
                "screen.png".into(),
                "image/png".into(),
                vec![0x89, 0x50, 0x4E, 0x47],
            );
            st.palette.open_root();
            st.palette.push_route(PaletteRoute::Attach);
            for _ in 0..120 {
                st.tick(1.0 / 120.0);
            }
            renderer.render(&mut st);
            let p = out_dir.join("07-palette-attach-chips.png");
            renderer.pixmap.save_png(&p).map_err(|e| e.to_string())?;
            written.push(p);
        }

        Ok(written)
    }
}

fn mask_field(field: &TextField) -> TextField {
    let count = field.text.chars().count();
    let dots: String = std::iter::repeat('•').take(count).collect();
    let char_idx = field.text[..field.cursor.min(field.text.len())]
        .chars()
        .count();
    let anchor_idx = field.text[..field.anchor.min(field.text.len())]
        .chars()
        .count();
    let dot_len = '•'.len_utf8();
    TextField {
        text: dots,
        cursor: char_idx * dot_len,
        anchor: anchor_idx * dot_len,
    }
}

fn truncate_label(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

#[inline]
fn blend_coverage_pixel(dst: &mut [u8], color: Rgba, coverage: u8) {
    let eff_a = ((color.a as u32) * (coverage as u32) + 127) / 255;
    if eff_a == 0 {
        return;
    }
    let sr = ((color.r as u32) * eff_a + 127) / 255;
    let sg = ((color.g as u32) * eff_a + 127) / 255;
    let sb = ((color.b as u32) * eff_a + 127) / 255;
    let inv = 255 - eff_a;
    dst[0] = (sr + ((dst[0] as u32) * inv + 127) / 255).min(255) as u8;
    dst[1] = (sg + ((dst[1] as u32) * inv + 127) / 255).min(255) as u8;
    dst[2] = (sb + ((dst[2] as u32) * inv + 127) / 255).min(255) as u8;
    dst[3] = (eff_a + ((dst[3] as u32) * inv + 127) / 255).min(255) as u8;
}

fn measure_buffer_width(buf: &Buffer) -> f32 {
    let mut w = 0.0f32;
    for run in buf.layout_runs() {
        if let Some(last) = run.glyphs.last() {
            w = w.max(last.x + last.w);
        }
    }
    w
}

fn byte_offset_to_line_col(text: &str, byte_offset: usize) -> (usize, usize) {
    let clamped = byte_offset.min(text.len());
    let prefix = &text[..clamped];
    let line = prefix.bytes().filter(|&b| b == b'\n').count();
    let col = match prefix.rfind('\n') {
        Some(nl) => clamped - (nl + 1),
        None => clamped,
    };
    (line, col)
}

fn glyph_x_for_byte(glyphs: &[cosmic_text::LayoutGlyph], col_byte: usize) -> f32 {
    for g in glyphs {
        if col_byte <= g.start {
            return g.x;
        }
        if col_byte < g.end {
            let span = (g.end - g.start).max(1) as f32;
            let frac = ((col_byte - g.start) as f32) / span;
            return g.x + g.w * frac;
        }
    }
    glyphs.last().map(|g| g.x + g.w).unwrap_or(0.0)
}

pub fn build_rrect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
    if r <= 0.25 {
        return tiny_skia::Rect::from_xywh(x, y, w, h).map(PathBuilder::from_rect);
    }
    const K: f32 = 0.552_284_8;
    let c = r * K;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + c, y, x + w, y + r - c, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(
        x + w,
        y + h - r + c,
        x + w - r + c,
        y + h,
        x + w - r,
        y + h,
    );
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - c, y + h, x, y + h - r + c, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - c, x + r - c, y, x + r, y);
    pb.close();
    pb.finish()
}

#[allow(clippy::too_many_arguments)]
fn blit_sub_pixmap(
    dst: &mut Pixmap,
    mut dst_x: i32,
    mut dst_y: i32,
    src: &Pixmap,
    mut src_x: u32,
    mut src_y: u32,
    mut w: u32,
    mut h: u32,
    opacity: f32,
) {
    let dw = dst.width() as i32;
    let dh = dst.height() as i32;

    if dst_x < 0 {
        let d = (-dst_x) as u32;
        if d >= w { return; }
        src_x += d;
        w -= d;
        dst_x = 0;
    }
    if dst_y < 0 {
        let d = (-dst_y) as u32;
        if d >= h { return; }
        src_y += d;
        h -= d;
        dst_y = 0;
    }
    if dst_x >= dw || dst_y >= dh {
        return;
    }
    w = w.min((dw - dst_x) as u32);
    h = h.min((dh - dst_y) as u32);
    if w == 0 || h == 0 {
        return;
    }

    let op = (opacity.clamp(0.0, 1.0) * 256.0).round() as u32;
    let src_stride = src.width() as usize * 4;
    let dst_stride = dst.width() as usize * 4;
    let src_data = src.data();
    let dst_data = dst.data_mut();

    for y in 0..h as usize {
        let s_row_off = (src_y as usize + y) * src_stride + src_x as usize * 4;
        let d_row_off = (dst_y as usize + y) * dst_stride + dst_x as usize * 4;
        let s_row = &src_data[s_row_off..s_row_off + w as usize * 4];
        let d_row = &mut dst_data[d_row_off..d_row_off + w as usize * 4];

        for x in 0..w as usize {
            let sa = (s_row[x * 4 + 3] as u32 * op) >> 8;
            if sa == 0 {
                continue;
            }
            let da = d_row[x * 4 + 3] as u32;
            if da == 0 {
                d_row[x * 4] = ((s_row[x * 4] as u32 * op) >> 8) as u8;
                d_row[x * 4 + 1] = ((s_row[x * 4 + 1] as u32 * op) >> 8) as u8;
                d_row[x * 4 + 2] = ((s_row[x * 4 + 2] as u32 * op) >> 8) as u8;
                d_row[x * 4 + 3] = sa as u8;
            } else {
                let inv_sa = 255 - sa;
                let sr = (s_row[x * 4] as u32 * op) >> 8;
                let sg = (s_row[x * 4 + 1] as u32 * op) >> 8;
                let sb = (s_row[x * 4 + 2] as u32 * op) >> 8;
                d_row[x * 4] = (sr + (d_row[x * 4] as u32 * inv_sa + 127) / 255) as u8;
                d_row[x * 4 + 1] = (sg + (d_row[x * 4 + 1] as u32 * inv_sa + 127) / 255) as u8;
                d_row[x * 4 + 2] = (sb + (d_row[x * 4 + 2] as u32 * inv_sa + 127) / 255) as u8;
                d_row[x * 4 + 3] = (sa + (da * inv_sa + 127) / 255) as u8;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_stretched_horizontal(
    dst: &mut Pixmap,
    mut dst_x: i32,
    mut dst_y: i32,
    mut dst_w: u32,
    src: &Pixmap,
    src_x: u32,
    mut src_y: u32,
    mut h: u32,
    opacity: f32,
) {
    let dw = dst.width() as i32;
    let dh = dst.height() as i32;

    if dst_x < 0 {
        let d = (-dst_x) as u32;
        if d >= dst_w { return; }
        dst_w -= d;
        dst_x = 0;
    }
    if dst_y < 0 {
        let d = (-dst_y) as u32;
        if d >= h { return; }
        src_y += d;
        h -= d;
        dst_y = 0;
    }
    if dst_x >= dw || dst_y >= dh {
        return;
    }
    dst_w = dst_w.min((dw - dst_x) as u32);
    h = h.min((dh - dst_y) as u32);
    if dst_w == 0 || h == 0 {
        return;
    }

    let op = (opacity.clamp(0.0, 1.0) * 256.0).round() as u32;
    let src_stride = src.width() as usize * 4;
    let dst_stride = dst.width() as usize * 4;
    let src_data = src.data();
    let dst_data = dst.data_mut();

    for y in 0..h as usize {
        let s_idx = (src_y as usize + y) * src_stride + src_x as usize * 4;
        let sa = (src_data[s_idx + 3] as u32 * op) >> 8;
        if sa == 0 {
            continue;
        }
        let sr = (src_data[s_idx] as u32 * op) >> 8;
        let sg = (src_data[s_idx + 1] as u32 * op) >> 8;
        let sb = (src_data[s_idx + 2] as u32 * op) >> 8;
        let inv_sa = 255 - sa;

        let d_row_off = (dst_y as usize + y) * dst_stride + dst_x as usize * 4;
        let d_row = &mut dst_data[d_row_off..d_row_off + dst_w as usize * 4];

        for x in 0..dst_w as usize {
            let da = d_row[x * 4 + 3] as u32;
            if da == 0 {
                d_row[x * 4] = sr as u8;
                d_row[x * 4 + 1] = sg as u8;
                d_row[x * 4 + 2] = sb as u8;
                d_row[x * 4 + 3] = sa as u8;
            } else {
                d_row[x * 4] = (sr + (d_row[x * 4] as u32 * inv_sa + 127) / 255) as u8;
                d_row[x * 4 + 1] = (sg + (d_row[x * 4 + 1] as u32 * inv_sa + 127) / 255) as u8;
                d_row[x * 4 + 2] = (sb + (d_row[x * 4 + 2] as u32 * inv_sa + 127) / 255) as u8;
                d_row[x * 4 + 3] = (sa + (da * inv_sa + 127) / 255) as u8;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_stretched_vertical(
    dst: &mut Pixmap,
    mut dst_x: i32,
    mut dst_y: i32,
    mut dst_h: u32,
    src: &Pixmap,
    mut src_x: u32,
    src_y: u32,
    mut w: u32,
    opacity: f32,
) {
    let dw = dst.width() as i32;
    let dh = dst.height() as i32;

    if dst_x < 0 {
        let d = (-dst_x) as u32;
        if d >= w { return; }
        src_x += d;
        w -= d;
        dst_x = 0;
    }
    if dst_y < 0 {
        let d = (-dst_y) as u32;
        if d >= dst_h { return; }
        dst_h -= d;
        dst_y = 0;
    }
    if dst_x >= dw || dst_y >= dh {
        return;
    }
    w = w.min((dw - dst_x) as u32);
    dst_h = dst_h.min((dh - dst_y) as u32);
    if w == 0 || dst_h == 0 {
        return;
    }

    let op = (opacity.clamp(0.0, 1.0) * 256.0).round() as u32;
    let src_stride = src.width() as usize * 4;
    let dst_stride = dst.width() as usize * 4;
    let s_row_off = src_y as usize * src_stride + src_x as usize * 4;
    let s_row = &src.data()[s_row_off..s_row_off + w as usize * 4];
    let dst_data = dst.data_mut();

    for y in 0..dst_h as usize {
        let d_row_off = (dst_y as usize + y) * dst_stride + dst_x as usize * 4;
        let d_row = &mut dst_data[d_row_off..d_row_off + w as usize * 4];

        for x in 0..w as usize {
            let sa = (s_row[x * 4 + 3] as u32 * op) >> 8;
            if sa == 0 {
                continue;
            }
            let da = d_row[x * 4 + 3] as u32;
            if da == 0 {
                d_row[x * 4] = ((s_row[x * 4] as u32 * op) >> 8) as u8;
                d_row[x * 4 + 1] = ((s_row[x * 4 + 1] as u32 * op) >> 8) as u8;
                d_row[x * 4 + 2] = ((s_row[x * 4 + 2] as u32 * op) >> 8) as u8;
                d_row[x * 4 + 3] = sa as u8;
            } else {
                let inv_sa = 255 - sa;
                let sr = (s_row[x * 4] as u32 * op) >> 8;
                let sg = (s_row[x * 4 + 1] as u32 * op) >> 8;
                let sb = (s_row[x * 4 + 2] as u32 * op) >> 8;
                d_row[x * 4] = (sr + (d_row[x * 4] as u32 * inv_sa + 127) / 255) as u8;
                d_row[x * 4 + 1] = (sg + (d_row[x * 4 + 1] as u32 * inv_sa + 127) / 255) as u8;
                d_row[x * 4 + 2] = (sb + (d_row[x * 4 + 2] as u32 * inv_sa + 127) / 255) as u8;
                d_row[x * 4 + 3] = (sa + (da * inv_sa + 127) / 255) as u8;
            }
        }
    }
}

fn reasoning_spans(text: &str, frontier: &[crate::stream::FrontierWord], color: Rgba) -> Vec<RichSpan> {
    let mut spans = Vec::new();
    let mut offset = 0;
    for (index, word) in frontier.iter().enumerate() {
        if word.start < offset || word.end > text.len() { continue; }
        if word.start > offset {
            spans.push(RichSpan { text: text[offset..word.start].into(), bold: false, italic: false,
                monospace: false, color, meta: 0 });
        }
        spans.push(RichSpan { text: text[word.start..word.end].into(), bold: false, italic: false,
            monospace: false, color, meta: crate::markdown::pack_meta(0, None, Some(index)) });
        offset = word.end;
    }
    if offset < text.len() {
        spans.push(RichSpan { text: text[offset..].into(), bold: false, italic: false, monospace: false, color, meta: 0 });
    }
    spans
}

fn cell_context(ctx: DrawContext, x: f32, width: f32) -> DrawContext {
    DrawContext { clip: ctx.clip.intersect(ClipBox {
        x0: ((x + 4.0) * ctx.scale).ceil() as i32,
        x1: ((x + width - 4.0) * ctx.scale).floor() as i32,
        ..ctx.clip
    }), ..ctx }
}

fn row_focus_opacity(buffers: &[Buffer], states: &[WordFocusState]) -> f32 {
    buffers.iter().flat_map(|buffer| buffer.layout_runs())
        .flat_map(|run| run.glyphs.iter())
        .filter_map(|glyph| unpack_frontier_idx(glyph.metadata).and_then(|i| states.get(i)))
        .map(|state| 0.35 + 0.65 * state.opacity).fold(1.0, f32::min)
}

#[cfg(test)]
mod rendering_regressions {
    use super::*;

    fn app() -> AppState {
        let mut config = AppConfig::default();
        config.model = "test-no-network".into();
        AppState::new(config)
    }

    #[test]
    fn vector_borders_and_fills_obey_the_same_viewport() {
        let mut renderer = Renderer::new(1.0);
        renderer.mask.data_mut().fill(255);
        let clip = ClipBox { x0: 20, y0: 25, x1: 90, y1: 75 };
        renderer.restrict_mask(clip);
        renderer.fill_rrect_logical(0.0, 0.0, 150.0, 150.0, 12.0, Rgba::hex(0xffffff), true);
        renderer.stroke_rrect_logical(10.0, 10.0, 100.0, 100.0, 8.0, 2.0, Rgba::hex(0xffffff));
        for (i, pixel) in renderer.pixmap.data().chunks_exact(4).enumerate() {
            let x = i % renderer.pixmap.width() as usize;
            let y = i / renderer.pixmap.width() as usize;
            if x < 20 || x >= 90 || y < 25 || y >= 75 { assert_eq!(pixel[3], 0); }
        }
    }

    #[test]
    fn unchanged_layout_is_shared_but_width_invalidates_it() {
        let mut renderer = Renderer::new(1.0);
        let mut app = app();
        app.messages.push(ChatItem::new_user("Cache this paragraph.".into()));
        let theme = app.theme;
        let first = renderer.cached_message(&app.messages[0], 0, 600.0, 0.0, &theme, None);
        let second = renderer.cached_message(&app.messages[0], 0, 600.0, 0.2, &theme, None);
        assert!(Rc::ptr_eq(&first, &second));
        let resized = renderer.cached_message(&app.messages[0], 0, 400.0, 0.2, &theme, None);
        assert!(!Rc::ptr_eq(&first, &resized));
        assert_eq!(renderer.message_cache.len(), 1);
    }

    #[test]
    fn code_is_unwrapped_and_wide_components_register_local_scroll() {
        let mut renderer = Renderer::new(1.0);
        let mut app = app();
        let code = format!("```rust\nlet line = \"{}\";\n```", "wide ".repeat(80));
        let mut reply = ChatItem::new_assistant_streaming(0.0);
        reply.stream = crate::stream::SmoothStream::from_completed(&code);
        reply.streaming = false;
        let scene = renderer.cached_message(&reply, 0, 500.0, 1.0, &app.theme, None);
        match &scene.blocks[0] {
            ShapedBlock::Code { code_buf, .. } => {
                assert_eq!(code_buf.layout_runs().count(), 1);
                assert!(measure_buffer_width(code_buf) > 500.0);
            }
            _ => panic!("expected code"),
        }
        renderer.mask.data_mut().fill(255);
        let ctx = DrawContext { scale: 1.0, alpha_mul: 1.0,
            clip: ClipBox { x0: 0, y0: 0, x1: 500, y1: 300 } };
        renderer.overflow_viewport(&mut app, (0, 0), LogicalRect::new(10.0, 10.0, 300.0, 100.0), 900.0, ctx);
        app.pointer_pos = Some((30.0, 30.0));
        app.scroll_input(80.0, 0.0, true, false);
        assert_eq!(app.block_scroll[&(0,0)], 80.0);
        app.scroll_input(0.0, 70.0, false, true);
        assert_eq!(app.block_scroll[&(0,0)], 150.0);
        assert_eq!(app.scroll_spring.target, 0.0);
    }
}

fn stress_markdown() -> String {
    format!("<think>Inspect wide content, preserve Unicode café العربية हिन्दी, and keep borders inside the viewport.</think>\n## Wide content\n\n| Item | Long identifier | Bytes | State | Owner | Detail |\n| --- | --- | --- | --- | --- | --- |\n| A | {} | 4096 | **Ready** | `renderer` | ==Clipped== |\n| B | {} | 8192 | Streaming | `parser` | No overflow |\n\n```rust\nlet very_long_line = \"{}\";\nfn main() {{ println!(\"café\"); }}\n```\n\n{}",
        "identifier_".repeat(12), "column_".repeat(10), "unwrapped_".repeat(50),
        "Paragraph **with emphasis**, `inline code`, and a [link](https://github.com).\n\n".repeat(8))
}
