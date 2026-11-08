use crate::anim::LogicalRect;
use crate::presentation::{PanelSnapshot, PanelTransition, TRANSITION_SECONDS};
use crate::blur::{draw_diffuse_rrect_shadow, BlurScratch};
use crate::config::{AppConfig, PRESETS};
use crate::markdown::{
    parse_markdown, unpack_frontier_idx, unpack_link_idx, ListMarker, MdBlock, ParsedMarkdown,
    RichSpan, META_CODESPAN, META_LINK, META_MARK, META_STRIKE,
};
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

pub struct Renderer {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    pub blur_scratch: BlurScratch,
    pub grain: GrainLut,
    pub pixmap: Pixmap,
    pub mask: Mask,
    pub scale: f32,
    panel_transition: PanelTransition,
    previous_panel: Option<PanelSnapshot>,
    outgoing_panel: Option<PanelSnapshot>,
    shadow_cache: Option<([u32; 7], Pixmap)>,
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
            previous_panel: None,
            outgoing_panel: None,
            message_cache: Vec::new(),
            shadow_cache: None,
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
            self.previous_panel = None;
            self.outgoing_panel = None;
            self.shadow_cache = None;
        }
    }

    pub fn render(&mut self, state: &mut AppState) {
        self.pixmap.fill(Color::TRANSPARENT);
        state.hit_zones.clear();
        state.scroll_regions.clear();

        let presence = state.presence_spring.value.clamp(0.0, 1.0);
        if !state.visible && presence <= 0.01 {
            return;
        }

        let scale = self.scale;
        let now = state.now_secs();
        let theme = state.theme;
        let panel = state.panel();
        if self.panel_transition.update(panel, now) {
            self.outgoing_panel = self.previous_panel.take();
            state.panel_transition_until = now + TRANSITION_SECONDS;
        }
        let panel_progress = self.panel_transition.progress(now);
        if panel_progress >= 1.0 { self.outgoing_panel = None; }

        // 1. Shape composer text first to compute exact dynamic bar height
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
        let desired_composer_h = (composer_text_h + 24.0).clamp(BAR_MIN_HEIGHT, 154.0);
        state.desired_composer_height = desired_composer_h;

        // 2. Shape above-bar content (Settings, Slash menu, or Conversation messages)
        // Measure at the destination width, not a moving spring value. Otherwise
        // line wrapping continually retargets height during a width morph.
        let inner_w = (target_w - 44.0).max(200.0);
        let mut shaped_msgs: Vec<Rc<ShapedMessage>> = Vec::new();
        let mut messages_total_h = 0.0f32;

        if panel == Panel::Settings {
            state.desired_content_height = self.estimate_settings_height(state);
        } else if panel == Panel::Commands {
            let count = state.filtered_slash_commands().len() as f32;
            state.desired_content_height = count * 36.0 + 14.0;
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

        // Shadow is a low-frequency signal: rasterize once in logical pixels,
        // cache while stationary, and bilinearly sample at output scale. Text
        // and card edges remain native-resolution. This bounds HiDPI blur cost.
        let key = [card.x.to_bits(), card.y.to_bits(), card.w.to_bits(), card.h.to_bits(),
            radius.to_bits(), theme.shadow_alpha.to_bits(), self.pixmap.height()];
        if self.shadow_cache.as_ref().map(|(old, _)| *old) != Some(key) {
            let mut shadow = Pixmap::new((self.pixmap.width() as f32 / scale).ceil() as u32,
                (self.pixmap.height() as f32 / scale).ceil() as u32).expect("shadow surface");
            for (offset, sigma, alpha) in [(3.0, 8.0, theme.shadow_alpha), (1.0, 2.5, theme.shadow_alpha * 0.42)] {
                draw_diffuse_rrect_shadow(&mut shadow.as_mut(), card.x, card.y, card.w, card.h,
                    radius, offset, sigma, alpha, (0,0,0));
            }
            self.shadow_cache = Some((key, shadow));
        }
        if let Some((_, shadow)) = &self.shadow_cache {
            self.pixmap.draw_pixmap(0, 0, shadow.as_ref(), &tiny_skia::PixmapPaint {
                opacity: presence, quality: tiny_skia::FilterQuality::Bilinear, ..Default::default()
            }, Transform::from_scale(scale, scale), None);
        }

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

        if above_h <= 4.0 { self.previous_panel = None; }
        if above_h > 4.0 {
            let content_fade = panel_progress;
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
                self.draw_slash_menu(state, card.x, composer_top, card.w, ctx);
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

            // Every route uses the same outgoing-content retention. Reversals
            // capture the currently composited scene, not a hard-coded source.
            self.retain_panel(card, composer_top, 1.0 - panel_progress);

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

    /// Composite the outgoing panel bottom-centered inside the current rounded
    /// card. No scaling of glyphs; shrink/expand is handled solely by geometry.
    fn retain_panel(&mut self, card: LogicalRect, composer_top: f32, outgoing_alpha: f32) {
        let scale = self.scale;
        let pw = self.pixmap.width() as usize;
        let ph = self.pixmap.height() as usize;
        let x0 = (card.x * scale).ceil().max(0.0) as usize;
        let x1 = ((card.x + card.w) * scale).floor().max(0.0).min(pw as f32) as usize;
        let y0 = (card.y * scale).ceil().max(0.0) as usize;
        let y1 = (composer_top * scale).floor().max(0.0).min(ph as f32) as usize;
        if x1 <= x0 || y1 <= y0 { return; }
        let w = x1 - x0;
        let h = y1 - y0;
        if let Some(old) = &self.outgoing_panel {
            let ox = (x0 + w / 2) as i32 - (old.width / 2) as i32;
            let oy = y1 as i32 - old.height as i32;
            let mask = self.mask.data();
            let dst = self.pixmap.data_mut();
            for y in y0..y1 {
                let sy = y as i32 - oy;
                if sy < 0 || sy >= old.height as i32 { continue; }
                for x in x0..x1 {
                    let sx = x as i32 - ox;
                    if sx < 0 || sx >= old.width as i32 { continue; }
                    let pixel = y * pw + x;
                    let k = outgoing_alpha * mask[pixel] as f32 / 255.0;
                    let si = (sy as usize * old.width + sx as usize) * 4;
                    let di = pixel * 4;
                    let inv = 1.0 - old.pixels[si + 3] as f32 / 255.0 * k;
                    for c in 0..4 {
                        dst[di + c] = (old.pixels[si + c] as f32 * k
                            + dst[di + c] as f32 * inv).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
        let mut pixels = self.previous_panel.take().map(|p| p.pixels).unwrap_or_default();
        pixels.clear();
        pixels.reserve(w * h * 4);
        for y in y0..y1 {
            pixels.extend_from_slice(&self.pixmap.data()[(y * pw + x0) * 4..(y * pw + x1) * 4]);
        }
        self.previous_panel = Some(PanelSnapshot { width: w, height: h, pixels });
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
                let n = ((self.grain.sample(px as usize, py as usize) as i32) * scale_amp) >> 6;
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

        state.hit_zones.push(HitZone {
            rect: LogicalRect::new(
                bar_rect.x + 6.0,
                bar_rect.y + 3.0,
                bar_rect.w - 12.0,
                bar_rect.h - 6.0,
            ),
            target: HitTarget::Input(FocusTarget::Composer),
        });

        let text_x = bar_rect.x + 18.0;
        let text_y = bar_rect.y + ((bar_rect.h - composer_text_h) * 0.5).max(11.0);

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

        let composer_focused =
            state.has_keyboard_focus && state.focus_target == FocusTarget::Composer;
        if composer_focused {
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

    fn draw_slash_menu(
        &mut self,
        state: &mut AppState,
        card_x: f32,
        composer_top: f32,
        card_w: f32,
        ctx: DrawContext,
    ) {
        let cmds = state.filtered_slash_commands();
        if cmds.is_empty() {
            return;
        }
        let sel = state.slash_selected.min(cmds.len().saturating_sub(1));
        let total_h = cmds.len() as f32 * 36.0 + 12.0;
        let start_y = composer_top - total_h + 6.0;
        let theme = state.theme;

        for (vis_idx, (_, entry)) in cmds.iter().enumerate() {
            let row_y = start_y + vis_idx as f32 * 36.0;
            let row_rect = LogicalRect::new(card_x + 10.0, row_y, card_w - 20.0, 32.0);
            state.hit_zones.push(HitZone {
                rect: row_rect,
                target: HitTarget::SlashItem(vis_idx),
            });

            let is_hovered =
                matches!(state.hovered_hit(), Some(HitTarget::SlashItem(i)) if i == vis_idx);
            if vis_idx == sel || is_hovered {
                self.fill_rrect_logical(
                    row_rect.x,
                    row_rect.y,
                    row_rect.w,
                    row_rect.h,
                    8.0,
                    theme.surface_hover.with_alpha(ctx.alpha_mul),
                    true,
                );
            }

            let (cmd_buf, _) =
                self.shape_plain_field(entry.cmd, 13.0, 18.0, 160.0, theme.fg, true, Wrap::None);
            let (lbl_buf, _) = self.shape_plain_field(
                entry.label,
                12.5,
                18.0,
                card_w - 160.0,
                theme.muted,
                false,
                Wrap::None,
            );
            self.draw_buffer_runs(
                &cmd_buf,
                row_rect.x + 12.0,
                row_rect.y + 7.0,
                &[],
                None,
                state,
                ctx,
            );
            self.draw_buffer_runs(
                &lbl_buf,
                row_rect.x + 114.0,
                row_rect.y + 7.0,
                &[],
                None,
                state,
                ctx,
            );
        }
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
        for (idx, (mode, label)) in [
            (ThemeMode::Dark, "Dark"),
            (ThemeMode::Light, "Light"),
        ]
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
            if active {
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
                    focus_border.with_alpha(ctx.alpha_mul),
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

        // Provider quick-select pills
        let mut px = pad_x;
        for (idx, preset) in PRESETS.iter().enumerate() {
            let (pbuf, _) = self.shape_plain_field(
                preset.label,
                11.5,
                16.0,
                140.0,
                theme.fg,
                false,
                Wrap::None,
            );
            let pw = (measure_buffer_width(&pbuf) + 18.0).max(52.0);
            if px + pw > card_x + card_w - 20.0 {
                break;
            }
            let prect = LogicalRect::new(px, y, pw, 24.0);
            state.hit_zones.push(HitZone {
                rect: prect,
                target: HitTarget::PresetPill(idx),
            });
            let is_match = state.field_base_url.text.trim().trim_end_matches('/')
                == preset.base_url.trim_end_matches('/');
            let is_hov = matches!(state.hovered_hit(), Some(HitTarget::PresetPill(i)) if i == idx);
            let fill = if is_match || is_hov {
                theme.surface_hover
            } else {
                theme.surface
            };
            self.fill_rrect_logical(
                prect.x,
                prect.y,
                prect.w,
                prect.h,
                7.0,
                fill.with_alpha(ctx.alpha_mul),
                true,
            );
            if is_match {
                self.stroke_rrect_logical(
                    prect.x,
                    prect.y,
                    prect.w,
                    prect.h,
                    7.0,
                    1.0,
                    focus_border.with_alpha(ctx.alpha_mul),
                );
            }
            self.draw_buffer_runs(&pbuf, prect.x + 9.0, prect.y + 4.0, &[], None, state, ctx);
            px += pw + 6.0;
        }

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
            Some(("Keyless", HitTarget::ToggleKeyMask, false))
        } else if state.show_api_key {
            Some(("Hide", HitTarget::ToggleKeyMask, true))
        } else {
            Some(("Show", HitTarget::ToggleKeyMask, true))
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
                    let selected = state.field_model.text.trim() == model_id;
                    self.fill_rrect_logical(
                        mrect.x,
                        mrect.y,
                        mrect.w,
                        mrect.h,
                        6.0,
                        if selected {
                            theme.surface_hover
                        } else {
                            theme.surface
                        }
                        .with_alpha(ctx.alpha_mul),
                        true,
                    );
                    if selected {
                        self.stroke_rrect_logical(
                            mrect.x,
                            mrect.y,
                            mrect.w,
                            mrect.h,
                            6.0,
                            1.0,
                            focus_border.with_alpha(ctx.alpha_mul),
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
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_labeled_settings_field(
        &mut self,
        state: &mut AppState,
        label: &str,
        target: FocusTarget,
        masked: bool,
        right_pill: Option<(&str, HitTarget, bool)>,
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
        if let Some((pill_txt, pill_hit, interactive)) = right_pill {
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
            }
            self.draw_buffer_runs(&pbuf, prect.x + 7.0, prect.y + 4.5, &[], None, state, ctx);
            right_reserve = pw + 14.0;
        }

        let field = match target {
            FocusTarget::SettingsBaseUrl => &state.field_base_url,
            FocusTarget::SettingsApiKey => &state.field_api_key,
            FocusTarget::SettingsModel => &state.field_model,
            FocusTarget::Composer => &state.composer,
        };

        let display_field: TextField = if masked && !field.text.is_empty() {
            let count = field.text.chars().count();
            let dots: String = std::iter::repeat('•').take(count).collect();
            let char_idx = field.text[..field.cursor.min(field.text.len())].chars().count();
            let anchor_idx = field.text[..field.anchor.min(field.text.len())].chars().count();
            let dot_len = '•'.len_utf8();
            TextField {
                text: dots,
                cursor: char_idx * dot_len,
                anchor: anchor_idx * dot_len,
            }
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

        let tx = box_rect.x + 11.0;
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
        theme.fg.r.hash(&mut hash); // theme is one of two immutable palettes
        (msg.role == Role::User).hash(&mut hash);
        if msg.role == Role::User { msg.raw_content.hash(&mut hash); }
        else { msg.stream.shown_text().hash(&mut hash); }
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
            let (buf, text_h) = self.shape_plain_field(
                &msg.raw_content,
                14.5,
                22.0,
                max_bubble_w - 28.0,
                theme.fg,
                false,
                Wrap::WordOrGlyph,
            );
            let text_w = measure_buffer_width(&buf).min(max_bubble_w - 28.0);
            let bubble_w = (text_w + 28.0).clamp(48.0, max_bubble_w);
            let bubble_h = (text_h + 18.0).max(38.0);
            return ShapedMessage {
                msg_idx,
                role: Role::User,
                user_buf: Some((buf, bubble_w, bubble_h)),
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
                            .max().unwrap_or(0) as f32 * 7.2 + 20.0;
                        (inner_w / cols as f32).max(natural.clamp(88.0, 360.0))
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
            let thumb_w = (rect.w * rect.w / content_w).max(24.0).min(rect.w);
            self.fill_rrect_logical(rect.x + (rect.w - thumb_w) * value / max_x, rect.y + rect.h - 2.0,
                thumb_w, 2.0, 1.0, state.theme.muted.with_alpha(ctx.alpha_mul * 0.35), true);
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
                    self.draw_buffer_runs(buf, bx + 14.0, my + 9.0, &[], None, state, mctx);
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
                    self.fill_rect_logical(
                        pad_x + 3.0,
                        my,
                        2.0,
                        th.min(visible_h),
                        theme.accent.with_alpha(mctx.alpha_mul * 0.40),
                        true,
                    );
                    let reasoning_states: Vec<_> = state.messages[sm.msg_idx].stream.reasoning_frontier.iter().map(|word| {
                        let t = ((now - word.birth_secs) / 0.24).clamp(0.0, 1.0) as f32;
                        WordFocusState { sigma_logical: 0.0, opacity: 0.45 + 0.55 * t * (2.0 - t), offset_y: 0.6 * (1.0 - t) }
                    }).collect();
                    let body_ctx = DrawContext { clip: mctx.clip.intersect(ClipBox {
                        x0: ((pad_x + 14.0) * ctx.scale).floor() as i32,
                        y0: (my * ctx.scale).floor() as i32,
                        x1: ((pad_x + inner_w) * ctx.scale).ceil() as i32,
                        y1: ((my + visible_h) * ctx.scale).floor() as i32,
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
                                mctx,
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
                        let mut cx = pad_x - offset;
                        for (c, cbuf) in header_cells.iter().enumerate() {
                            self.draw_buffer_runs(
                                cbuf,
                                cx + 8.0,
                                my + 6.0,
                                &frontier_states,
                                Some(*msg_idx),
                                state,
                                table_ctx,
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
                                theme.line.with_alpha(mctx.alpha_mul),
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
                                    table_ctx,
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
        buffer.set_size(&mut self.font_system, Some(max_w.max(20.0)), None);
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
                            state.hit_zones.push(HitZone {
                                rect: LogicalRect::new(start_x, pill_y, span_w, pill_h),
                                target: HitTarget::LinkUrl { msg_idx, link_idx },
                            });
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
        let mut placements: Vec<(cosmic_text::CacheKey, i32, i32, u32, u32)> =
            Vec::with_capacity(group.len());

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
                    placements.push((phys.cache_key, gx, gy, gw, gh));
                }
            }
        }

        if placements.is_empty() || max_x <= min_x || max_y <= min_y {
            return;
        }

        let word_w = (max_x - min_x) as usize;
        let word_h = (max_y - min_y) as usize;
        if word_w > 2048 || word_h > 512 {
            return;
        }

        let pad = ((sigma_px * 2.65).ceil() as usize).clamp(2, 48);
        let bw = word_w + pad * 2;
        let bh = word_h + pad * 2;

        {
            let src_mask = self.blur_scratch.prepare_mask(bw, bh);
            for (ck, gx, gy, gw, gh) in placements {
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

    pub fn render_showcase_pngs(out_dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
        std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
        let mut renderer = Self::new(2.0);
        let mut written = Vec::new();

        // 1. Minimalist Thin Floating Bar (zero placeholders, zero tooltips)
        {
            let mut st = AppState::new(AppConfig::default());
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
            let mut st = AppState::new(AppConfig::default());
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
            let mut st = AppState::new(cfg);
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
            let mut st = AppState::new(AppConfig::default());
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

        Ok(written)
    }
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
