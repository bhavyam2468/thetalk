use crate::anim::{LogicalRect, RectSpring, Spring, SpringSpec};
use crate::attach::{
    paste_clipboard_attachment_async, prepare_attachment_async, rules, validate_before_attach,
    AttachEvent, Attachment, AttachmentKind, AttachmentPayload, AttachmentStatus,
    PreparedAttachment,
};
use crate::config::{ApiFormat, AppConfig};
use crate::llm::{fetch_models_async, spawn_chat_stream, ChatTurnMsg, LlmEvent};
use crate::palette::{
    expand_tilde, profile_from_dropdown_selection, DropdownKind, PaletteRoute, PaletteRow,
    PaletteRowAction, PaletteState, ProviderField,
};
use crate::presentation::RouteKey;
use crate::stream::SmoothStream;
use crate::theme::{Theme, ThemeMode};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
    Arc,
};
use std::time::Instant;

/// The single presentation route. Priority lives here, not in each renderer/input branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Compact,
    Conversation,
    Commands,
    Settings,
}

/// Shared horizontal overflow viewport for any wide Markdown component.
#[derive(Debug, Clone)]
pub struct ScrollRegion {
    pub rect: LogicalRect,
    pub key: (usize, usize),
    pub max_x: f32,
}

/// Geometry constants (logical px)
pub const SHADOW_PAD_X: f32 = 36.0;
pub const SHADOW_PAD_TOP: f32 = 38.0;
pub const SHADOW_PAD_BOTTOM: f32 = 26.0;

pub const BAR_WIDTH_COMPACT: f32 = 540.0;
pub const BAR_WIDTH_EXPANDED: f32 = 684.0;
pub const BAR_MIN_HEIGHT: f32 = 46.0;
pub const CARD_MAX_HEIGHT: f32 = 640.0;

pub const SURFACE_WIDTH: u32 = (BAR_WIDTH_EXPANDED as u32) + (SHADOW_PAD_X as u32) * 2;
pub const SURFACE_HEIGHT: u32 =
    (CARD_MAX_HEIGHT as u32) + (SHADOW_PAD_TOP as u32) + (SHADOW_PAD_BOTTOM as u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    Composer,
    PaletteSearch,
    PaletteField(ProviderField),
    SettingsProfile,
    SettingsProtocol,
    SettingsVision,
    SettingsTheme(ThemeMode),
    SettingsBaseUrl,
    SettingsApiKey,
    SettingsKeyMask,
    SettingsModel,
    SettingsRefresh,
    SettingsModelChip(usize),
    SettingsNewProfile,
    SettingsDeleteProfile,
    SettingsDone,
}

impl FocusTarget {
    pub fn is_text_input(self) -> bool {
        matches!(
            self,
            Self::Composer
                | Self::PaletteSearch
                | Self::PaletteField(_)
                | Self::SettingsBaseUrl
                | Self::SettingsApiKey
                | Self::SettingsModel
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    Input(FocusTarget),
    SendOrStop,
    PaletteBack,
    PaletteSearchBox,
    PaletteRow(usize),
    PaletteDropdownOption(usize),
    ComposerAttachChipRemove(usize),
    SettingsProfileToggle,
    SettingsProfileOption(usize),
    SettingsProtocolToggle,
    SettingsVisionToggle,
    SettingsNewProfile,
    SettingsDeleteProfile,
    ModelChip(usize),
    RefreshModels,
    ToggleKeyMask,
    ThemeToggle(ThemeMode),
    SettingsClose,
    OpenSettings,
    CopyCode { msg_idx: usize, block_idx: usize },
    #[allow(dead_code)]
    CopyMessage(usize),
    #[allow(dead_code)]
    RegenerateLast,
    ToggleThinking(usize),
    LinkUrl { msg_idx: usize, link_idx: usize },
}

#[derive(Debug, Clone)]
pub struct HitZone {
    pub rect: LogicalRect,
    pub target: HitTarget,
}

#[derive(Debug, Clone)]
pub struct TextField {
    pub text: String,
    pub cursor: usize,
    pub anchor: usize,
}

impl Default for TextField {
    fn default() -> Self {
        Self::new("")
    }
}

impl TextField {
    pub fn new(initial: &str) -> Self {
        let len = initial.len();
        Self {
            text: initial.to_string(),
            cursor: len,
            anchor: len,
        }
    }

    pub fn set(&mut self, value: impl Into<String>) {
        self.text = value.into();
        self.cursor = self.text.len();
        self.anchor = self.cursor;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.anchor = 0;
    }

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        if self.cursor == self.anchor {
            None
        } else if self.cursor < self.anchor {
            Some((self.cursor, self.anchor))
        } else {
            Some((self.anchor, self.cursor))
        }
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection_range().map(|(a, b)| &self.text[a..b])
    }

    pub fn delete_selection(&mut self) -> bool {
        if let Some((a, b)) = self.selection_range() {
            self.text.replace_range(a..b, "");
            self.cursor = a;
            self.anchor = a;
            true
        } else {
            false
        }
    }

    pub fn insert_str(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.anchor = self.cursor;
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor > 0 {
            let prev = prev_char_boundary(&self.text, self.cursor);
            self.text.replace_range(prev..self.cursor, "");
            self.cursor = prev;
            self.anchor = prev;
        }
    }

    pub fn delete_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor < self.text.len() {
            let next = next_char_boundary(&self.text, self.cursor);
            self.text.replace_range(self.cursor..next, "");
        }
    }

    pub fn delete_word_back(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let target = prev_word_boundary(&self.text, self.cursor);
        self.text.replace_range(target..self.cursor, "");
        self.cursor = target;
        self.anchor = target;
    }

    pub fn move_left(&mut self, extend: bool) {
        if !extend {
            if let Some((a, _)) = self.selection_range() {
                self.cursor = a;
                self.anchor = a;
                return;
            }
        }
        self.cursor = prev_char_boundary(&self.text, self.cursor);
        if !extend {
            self.anchor = self.cursor;
        }
    }

    pub fn move_right(&mut self, extend: bool) {
        if !extend {
            if let Some((_, b)) = self.selection_range() {
                self.cursor = b;
                self.anchor = b;
                return;
            }
        }
        self.cursor = next_char_boundary(&self.text, self.cursor);
        if !extend {
            self.anchor = self.cursor;
        }
    }

    pub fn move_word_left(&mut self, extend: bool) {
        self.cursor = prev_word_boundary(&self.text, self.cursor);
        if !extend {
            self.anchor = self.cursor;
        }
    }

    pub fn move_word_right(&mut self, extend: bool) {
        self.cursor = next_word_boundary(&self.text, self.cursor);
        if !extend {
            self.anchor = self.cursor;
        }
    }

    pub fn move_home(&mut self, extend: bool) {
        let line_start = self.text[..self.cursor]
            .rfind('\n')
            .map(|idx| idx + 1)
            .unwrap_or(0);
        self.cursor = line_start;
        if !extend {
            self.anchor = self.cursor;
        }
    }

    pub fn move_end(&mut self, extend: bool) {
        let line_end = self.text[self.cursor..]
            .find('\n')
            .map(|rel| self.cursor + rel)
            .unwrap_or(self.text.len());
        self.cursor = line_end;
        if !extend {
            self.anchor = self.cursor;
        }
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.text.len();
    }
}

fn prev_char_boundary(s: &str, idx: usize) -> usize {
    if idx == 0 {
        return 0;
    }
    let mut i = idx.min(s.len()) - 1;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx + 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn prev_word_boundary(s: &str, idx: usize) -> usize {
    let bytes = s.as_bytes();
    let mut i = idx.min(bytes.len());
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    while i > 0 && !bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_word_boundary(s: &str, idx: usize) -> usize {
    let bytes = s.as_bytes();
    let mut i = idx.min(bytes.len());
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    while i < bytes.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

pub struct ChatItem {
    pub role: Role,
    pub raw_content: String,
    pub attachments: Vec<AttachmentPayload>,
    pub stream: SmoothStream,
    pub streaming: bool,
    pub error: Option<String>,
    pub thinking_expanded: bool,
    pub thinking_reveal: Spring,
    pub entrance: Spring,
}

impl ChatItem {
    pub fn new_user(text: String) -> Self {
        Self::new_user_with_attachments(text, Vec::new())
    }

    pub fn new_user_with_attachments(text: String, attachments: Vec<AttachmentPayload>) -> Self {
        let s = SmoothStream::from_completed(&text);
        let mut entrance = Spring::new(0.0, SpringSpec::FAST_SPATIAL);
        entrance.retarget(1.0);
        Self {
            role: Role::User,
            raw_content: text,
            attachments,
            stream: s,
            streaming: false,
            error: None,
            thinking_expanded: false,
            thinking_reveal: Spring::new(0.0, SpringSpec::DEFAULT_SPATIAL),
            entrance,
        }
    }

    pub fn new_assistant_streaming(now_secs: f64) -> Self {
        let mut s = SmoothStream::new();
        s.start_streaming(now_secs);
        let mut entrance = Spring::new(0.0, SpringSpec::FAST_SPATIAL);
        entrance.retarget(1.0);
        Self {
            role: Role::Assistant,
            raw_content: String::new(),
            attachments: Vec::new(),
            stream: s,
            streaming: true,
            error: None,
            thinking_expanded: false,
            thinking_reveal: Spring::new(0.0, SpringSpec::DEFAULT_SPATIAL),
            entrance,
        }
    }
}

pub struct AppState {
    pub start_instant: Instant,
    pub config: AppConfig,
    pub theme: Theme,

    // Visibility & Quit
    pub visible: bool,
    pub should_quit: bool,
    pub presence_spring: Spring,

    // Animated outer card geometry in surface-local logical coordinates
    pub card_rect: RectSpring,
    pub card_radius: Spring,
    pub desired_content_height: f32,
    pub desired_composer_height: f32,

    // Scroll physics for message viewport
    pub scroll_spring: Spring,
    pub max_scroll: f32,
    pub block_scroll: std::collections::HashMap<(usize, usize), f32>,
    pub scroll_regions: Vec<ScrollRegion>,
    pub user_scrolled_up: bool,

    // Inputs
    pub composer: TextField,
    pub focus_target: FocusTarget,
    pub last_cursor_activity: f64,

    // Hierarchical Command Palette
    pub palette: PaletteState,
    pub palette_highlight_y: Spring,
    pub palette_dropdown_highlight_y: Spring,

    // Pending composer attachments + async preparation
    pub composer_attachments: Vec<Attachment>,
    pub attach_status: Option<(String, f64)>,
    next_attach_id: u64,
    attach_tx: Sender<AttachEvent>,
    attach_rx: Receiver<AttachEvent>,

    // Settings state
    pub settings_open: bool,
    pub settings_profile_dropdown: bool,
    pub settings_profile_selected: usize,
    pub panel_transition_until: f64,
    pub summon_generation: u64,
    pub field_base_url: TextField,
    pub field_api_key: TextField,
    pub field_model: TextField,
    pub show_api_key: bool,
    pub discovered_models: Vec<String>,
    pub loading_models: bool,
    pub models_status: Option<String>,

    // Chat & streaming channels
    pub messages: Vec<ChatItem>,
    llm_tx: Sender<LlmEvent>,
    llm_rx: Receiver<LlmEvent>,
    active_req_id: Option<u64>,
    active_cancel: Option<Arc<AtomicBool>>,
    next_req_id: u64,
    pub send_bloom: Spring,

    // Copy feedback badge `(msg_idx, block_idx, expire_secs)`
    pub copied_badge: Option<(usize, usize, f64)>,
    internal_clipboard: String,

    // Pointer & Hitboxes
    pub pointer_pos: Option<(f32, f32)>,
    pub hit_zones: Vec<HitZone>,
    pub palette_viewport_rect: Option<LogicalRect>,

    // `niri` Zero-Hijack Focus State Machine
    pub has_keyboard_focus: bool,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        let theme = Theme::for_mode(config.theme);
        let initial_rect = Self::compute_target_rect(false, 0.0, BAR_MIN_HEIGHT);
        let mut presence = Spring::new(0.88, SpringSpec::FAST_SPATIAL);
        presence.retarget(1.0);

        let field_base_url = TextField::new(&config.base_url);
        let field_api_key = TextField::new(&config.api_key);
        let field_model = TextField::new(&config.model);
        let (llm_tx, llm_rx) = mpsc::channel();
        let (attach_tx, attach_rx) = mpsc::channel();

        let mut app = Self {
            start_instant: Instant::now(),
            config,
            theme,
            visible: true,
            should_quit: false,
            presence_spring: presence,
            card_rect: RectSpring::new(initial_rect, SpringSpec::SHEET_MORPH),
            card_radius: Spring::new(23.0, SpringSpec::FAST_SPATIAL),
            desired_content_height: 0.0,
            desired_composer_height: BAR_MIN_HEIGHT,
            scroll_spring: Spring::new(0.0, SpringSpec::SCROLL),
            max_scroll: 0.0,
            block_scroll: std::collections::HashMap::new(),
            scroll_regions: Vec::new(),
            user_scrolled_up: false,
            composer: TextField::default(),
            focus_target: FocusTarget::Composer,
            last_cursor_activity: 0.0,
            palette: PaletteState::new(),
            palette_highlight_y: Spring::new(0.0, SpringSpec::FAST_SPATIAL),
            palette_dropdown_highlight_y: Spring::new(0.0, SpringSpec::FAST_SPATIAL),
            composer_attachments: Vec::new(),
            attach_status: None,
            next_attach_id: 1,
            attach_tx,
            attach_rx,
            settings_open: false,
            settings_profile_dropdown: false,
            settings_profile_selected: 0,
            panel_transition_until: 0.0,
            summon_generation: 0,
            field_base_url,
            field_api_key,
            field_model,
            show_api_key: false,
            discovered_models: Vec::new(),
            loading_models: false,
            models_status: None,
            messages: Vec::new(),
            llm_tx,
            llm_rx,
            active_req_id: None,
            active_cancel: None,
            next_req_id: 1,
            send_bloom: Spring::new(0.0, SpringSpec::FAST_SPATIAL),
            copied_badge: None,
            internal_clipboard: String::new(),
            pointer_pos: None,
            hit_zones: Vec::new(),
            palette_viewport_rect: None,
            has_keyboard_focus: false,
        };

        if app.config.model.trim().is_empty() {
            app.trigger_model_discovery(true);
        }
        app
    }

    pub fn now_secs(&self) -> f64 {
        self.start_instant.elapsed().as_secs_f64()
    }

    pub fn is_streaming(&self) -> bool {
        self.active_req_id.is_some()
    }

    pub fn composer_has_slash_trigger(&self) -> bool {
        let trimmed = self.composer.text.trim_start();
        trimmed.starts_with('/') && !trimmed.contains(' ') && !trimmed.contains('\n')
    }

    pub fn slash_menu_open(&self) -> bool {
        !self.settings_open && (self.palette.open || self.composer_has_slash_trigger())
    }

    pub fn panel(&self) -> Panel {
        if self.settings_open {
            Panel::Settings
        } else if self.slash_menu_open() {
            Panel::Commands
        } else if !self.messages.is_empty() {
            Panel::Conversation
        } else {
            Panel::Compact
        }
    }

    pub fn route_key(&self) -> RouteKey {
        let p = self.panel();
        self.palette.route_key(p)
    }

    pub fn is_expanded(&self) -> bool {
        self.panel() != Panel::Compact || !self.composer_attachments.is_empty()
    }

    pub fn palette_rows(&self) -> Vec<PaletteRow> {
        let has_reasoning = self
            .messages
            .iter()
            .any(|m| m.stream.reasoning.as_deref().is_some_and(|s| !s.trim().is_empty()));
        let has_assistant_reply = self
            .messages
            .iter()
            .any(|m| m.role == Role::Assistant && !m.stream.full_answer().trim().is_empty());
        self.palette.build_rows(
            &self.composer.text,
            &self.config,
            &self.discovered_models,
            self.loading_models,
            self.show_api_key,
            &self.composer_attachments,
            !self.messages.is_empty(),
            has_reasoning,
            has_assistant_reply,
        )
    }

    fn compute_target_rect(expanded: bool, above_bar_h: f32, composer_h: f32) -> LogicalRect {
        let width = if expanded {
            BAR_WIDTH_EXPANDED
        } else {
            BAR_WIDTH_COMPACT
        };
        let total_h = (above_bar_h + composer_h).clamp(BAR_MIN_HEIGHT, CARD_MAX_HEIGHT);
        let x = (SURFACE_WIDTH as f32 - width) * 0.5;
        let bottom = SURFACE_HEIGHT as f32 - SHADOW_PAD_BOTTOM;
        let y = (bottom - total_h).max(SHADOW_PAD_TOP);
        LogicalRect::new(x, y, width, total_h)
    }

    pub fn sync_geometry_targets(&mut self) {
        let expanded = self.is_expanded();
        let composer_h = self.desired_composer_height.clamp(BAR_MIN_HEIGHT, 210.0);
        let max_above = CARD_MAX_HEIGHT - composer_h;
        let above_h = self.desired_content_height.clamp(0.0, max_above);
        let target = Self::compute_target_rect(expanded, above_h, composer_h);
        self.card_rect.retarget(target);
        self.card_radius.retarget(if expanded { 16.0 } else { 23.0 });

        let show_send = !self.composer.text.trim().is_empty()
            || !self.composer_attachments.is_empty()
            || self.is_streaming();
        self.send_bloom.retarget(if show_send { 1.0 } else { 0.0 });
    }

    fn sync_palette_highlight_target(&mut self, rows: &[PaletteRow]) {
        let sel = self
            .palette
            .current_frame()
            .map(|f| f.selected)
            .unwrap_or(0);
        let scroll = self
            .palette
            .current_frame()
            .map(|f| f.scroll_row)
            .unwrap_or(0);
        if !rows.is_empty() {
            let rel = sel.saturating_sub(scroll) as f32;
            self.palette_highlight_y
                .retarget(rel * crate::palette::ROW_STEP);
        }
        if let Some(dd) = self.palette.dropdown {
            let opts = self.palette.build_dropdown_options(dd, &self.config);
            if !opts.is_empty() {
                let d_rel = self
                    .palette
                    .dropdown_selected
                    .saturating_sub(self.palette.dropdown_scroll) as f32;
                self.palette_dropdown_highlight_y
                    .retarget(d_rel * crate::palette::ROW_STEP);
            }
        }
    }

    /// Advances all physics springs, streaming token revealers, and background events.
    /// Returns `true` if another animation frame should be scheduled immediately.
    pub fn tick(&mut self, dt: f32) -> bool {
        let now = self.now_secs();

        // Drain async attachment preparations
        let mut any_attach_update = false;
        while let Ok(ev) = self.attach_rx.try_recv() {
            any_attach_update = true;
            match ev {
                AttachEvent::Prepared { id, result } => {
                    if let Some(pos) = self.composer_attachments.iter().position(|a| a.id == id) {
                        if let Some(err) = result.error {
                            self.attach_status = Some((err, now + 3.2));
                            self.composer_attachments.remove(pos);
                        } else {
                            self.composer_attachments[pos].apply_prepared(result);
                        }
                    }
                }
                AttachEvent::ClipboardResult { result } => match result {
                    Ok(path) => {
                        self.attach_file_path(path);
                    }
                    Err(err) => {
                        self.attach_status = Some((err, now + 2.5));
                    }
                },
            }
        }
        for att in &mut self.composer_attachments {
            if att.entrance.advance(dt) {
                any_attach_update = true;
            }
        }
        if let Some((_, exp)) = self.attach_status {
            if now >= exp {
                self.attach_status = None;
            } else {
                any_attach_update = true;
            }
        }

        // Poll LLM events (`ModelsFetched`, `ModelAutoSelected`, `Delta`, `Finished`, `Error`)
        let events: Vec<_> = self.llm_rx.try_iter().take(128).collect();
        let last_snapshot = events.iter().rposition(|event| match event {
            LlmEvent::Delta { req_id, .. } | LlmEvent::Finished { req_id, .. } => {
                Some(*req_id) == self.active_req_id
            }
            _ => false,
        });
        for (index, ev) in events.into_iter().enumerate() {
            if matches!(&ev, LlmEvent::Delta { req_id, .. } if Some(*req_id) == self.active_req_id)
                && Some(index) != last_snapshot
            {
                continue;
            }
            match ev {
                LlmEvent::ModelsFetched(res) => {
                    self.loading_models = false;
                    match res {
                        Ok(list) => {
                            if self.field_model.text.trim().is_empty() {
                                if let Some(first) = list.first() {
                                    self.field_model.set(first.clone());
                                    self.config.model = first.clone();
                                    let _ = self.config.save();
                                }
                            }
                            self.models_status = if list.is_empty() {
                                Some("No models loaded on server".to_string())
                            } else {
                                None
                            };
                            self.discovered_models = list;
                        }
                        Err(err) => {
                            if self.settings_open || self.slash_menu_open() {
                                self.models_status = Some(err);
                            }
                        }
                    }
                }
                LlmEvent::ModelAutoSelected { req_id, model } => {
                    if self.active_req_id == Some(req_id) {
                        self.config.model = model.clone();
                        self.field_model.set(model);
                        let _ = self.config.save();
                    }
                }
                LlmEvent::Delta { req_id, full_raw } => {
                    if self.active_req_id == Some(req_id) {
                        if let Some(last) = self.messages.last_mut() {
                            last.raw_content = full_raw.clone();
                            last.stream.update_raw(&full_raw, true, now);
                        }
                    }
                }
                LlmEvent::Finished { req_id, full_raw } => {
                    if self.active_req_id == Some(req_id) {
                        if let Some(last) = self.messages.last_mut() {
                            last.raw_content = full_raw.clone();
                            last.stream.update_raw(&full_raw, false, now);
                            last.streaming = false;
                        }
                        self.active_req_id = None;
                        self.active_cancel = None;
                    }
                }
                LlmEvent::Error { req_id, message } => {
                    if self.active_req_id == Some(req_id) {
                        if let Some(last) = self.messages.last_mut() {
                            let cur = last.raw_content.clone();
                            last.stream.update_raw(&cur, false, now);
                            last.streaming = false;
                            last.error = Some(message);
                        }
                        self.active_req_id = None;
                        self.active_cancel = None;
                    }
                }
            }
        }

        // Advance smooth word reveal & optical blur frontier
        let mut any_stream_anim = false;
        for msg in &mut self.messages {
            let was_shown = msg.stream.shown_text().len();
            if msg.stream.tick(dt as f64, now) {
                any_stream_anim = true;
            }
            if msg.stream.shown_text().len() != was_shown && !self.user_scrolled_up {
                self.scroll_spring.retarget(self.max_scroll);
            }
            if msg.streaming {
                any_stream_anim = true;
            }
            msg.thinking_reveal
                .retarget(if msg.thinking_expanded { 1.0 } else { 0.0 });
            if msg.thinking_reveal.advance(dt) {
                any_stream_anim = true;
            }
            if msg.entrance.advance(dt) {
                any_stream_anim = true;
            }
        }

        if let Some((_, _, exp)) = self.copied_badge {
            if now >= exp {
                self.copied_badge = None;
            } else {
                any_stream_anim = true;
            }
        }

        if self.slash_menu_open() {
            if self.palette.stack.is_empty() {
                self.palette
                    .stack
                    .push(crate::palette::PaletteFrame::new(PaletteRoute::Root));
            }
            let rows = self.palette_rows();
            self.palette.clamp_selection(rows.len());
            self.sync_palette_highlight_target(&rows);
        } else if !self.palette.stack.is_empty() {
            self.palette.close();
        }

        self.sync_geometry_targets();

        let mut moving = false;
        moving |= self.presence_spring.advance(dt);
        moving |= self.card_rect.advance(dt);
        moving |= self.card_radius.advance(dt);
        moving |= self.scroll_spring.advance(dt);
        moving |= self.send_bloom.advance(dt);
        moving |= self.palette_highlight_y.advance(dt);
        moving |= self.palette_dropdown_highlight_y.advance(dt);

        if !self.visible
            && !self.presence_spring.is_running()
            && self.presence_spring.value <= 0.02
        {
            return false;
        }

        moving
            || any_stream_anim
            || any_attach_update
            || self.loading_models
            || now < self.panel_transition_until
    }

    pub fn wants_keyboard_on_demand(&self) -> bool {
        self.visible
    }

    pub fn hovered_hit(&self) -> Option<HitTarget> {
        let (px, py) = self.pointer_pos?;
        self.hit_zones
            .iter()
            .rev()
            .find(|z| z.rect.contains(px, py))
            .map(|z| z.target)
    }

    pub fn active_field_mut(&mut self) -> &mut TextField {
        if self.slash_menu_open() {
            if self.palette.editing_field.is_some() {
                return &mut self.palette.edit_buffer;
            }
            if self.palette.has_search_bar()
                || (self.palette.open && self.palette.is_at_root())
            {
                if let Some(frame) = self.palette.current_frame_mut() {
                    return &mut frame.query;
                }
            }
            return &mut self.composer;
        }
        match self.focus_target {
            FocusTarget::SettingsBaseUrl => &mut self.field_base_url,
            FocusTarget::SettingsApiKey => &mut self.field_api_key,
            FocusTarget::SettingsModel => &mut self.field_model,
            FocusTarget::PaletteField(_) => &mut self.palette.edit_buffer,
            FocusTarget::PaletteSearch => {
                if let Some(frame) = self.palette.current_frame_mut() {
                    &mut frame.query
                } else {
                    &mut self.composer
                }
            }
            _ => &mut self.composer,
        }
    }

    pub fn sync_fields_from_config(&mut self) {
        self.field_base_url.set(self.config.base_url.clone());
        self.field_api_key.set(self.config.api_key.clone());
        self.field_model.set(self.config.model.clone());
    }

    pub fn open_settings(&mut self) {
        self.palette.close();
        self.settings_open = true;
        self.settings_profile_dropdown = false;
        self.sync_fields_from_config();
        self.focus_target = FocusTarget::SettingsBaseUrl;
        self.request_focus();
        self.trigger_model_discovery(false);
    }

    pub fn save_and_close_settings(&mut self) {
        self.commit_settings_fields();
        self.settings_open = false;
        self.settings_profile_dropdown = false;
        self.focus_target = FocusTarget::Composer;
        self.last_cursor_activity = self.now_secs();
    }

    fn commit_settings_fields(&mut self) {
        let url = self.field_base_url.text.trim().to_string();
        let key = self.field_api_key.text.trim().to_string();
        let model = self.field_model.text.trim().to_string();
        let url = if url.is_empty() {
            "http://localhost:11434/v1".to_string()
        } else {
            url
        };
        let fmt = self.config.active_format();
        let active_name = self.config.active_profile_label().to_string();
        self.config
            .update_active_profile(&active_name, fmt, &url, &key, &model);
        let _ = self.config.save();
        self.sync_fields_from_config();
    }

    pub fn select_profile_by_id(&mut self, id: &str) {
        if self.config.select_profile(id) {
            let _ = self.config.save();
            self.sync_fields_from_config();
            self.discovered_models.clear();
            self.models_status = None;
            self.trigger_model_discovery(false);
        }
    }

    pub fn create_custom_profile_and_select(&mut self) {
        let n = self.config.custom_profiles().len() + 1;
        let name = format!("Custom {}", n);
        let fmt = self.config.active_format();
        let url = self.config.base_url.clone();
        self.config.add_custom_profile(&name, fmt, &url, "");
        let _ = self.config.save();
        self.sync_fields_from_config();
        self.discovered_models.clear();
    }

    pub fn delete_active_custom_profile(&mut self) {
        let id = self.config.active_profile_id.clone();
        if self.config.delete_custom_profile(&id) {
            let _ = self.config.save();
            self.sync_fields_from_config();
            self.discovered_models.clear();
            self.trigger_model_discovery(false);
        }
    }

    pub fn toggle_protocol_format(&mut self, fmt: Option<ApiFormat>) {
        let cur = self.config.active_format();
        let next = fmt.unwrap_or(match cur {
            ApiFormat::OpenAi => ApiFormat::Anthropic,
            ApiFormat::Anthropic => ApiFormat::OpenAi,
        });
        let name = self.config.active_profile_label().to_string();
        let url = if next == ApiFormat::Anthropic
            && (self.config.base_url.contains("11434")
                || self.config.base_url.contains("api.openai.com"))
        {
            "https://api.anthropic.com/v1".to_string()
        } else if next == ApiFormat::OpenAi && self.config.base_url.contains("api.anthropic.com") {
            "https://api.openai.com/v1".to_string()
        } else {
            self.config.base_url.clone()
        };
        let key = self.config.api_key.clone();
        let model = self.config.model.clone();
        self.config
            .update_active_profile(&name, next, &url, &key, &model);
        let _ = self.config.save();
        self.sync_fields_from_config();
        self.discovered_models.clear();
        self.trigger_model_discovery(false);
    }

    pub fn toggle_vision(&mut self) {
        self.config.vision = !self.config.vision;
        let _ = self.config.save();
    }

    pub fn trigger_model_discovery(&mut self, silent: bool) {
        let mut probe_cfg = self.config.clone();
        if self.settings_open {
            probe_cfg.base_url = self.field_base_url.text.trim().to_string();
            probe_cfg.api_key = self.field_api_key.text.trim().to_string();
        }
        if probe_cfg.base_url.is_empty() {
            return;
        }
        self.loading_models = true;
        if !silent {
            self.models_status = Some("Checking endpoint...".to_string());
        }
        fetch_models_async(probe_cfg, self.llm_tx.clone(), None);
    }

    pub fn set_theme_mode(&mut self, mode: ThemeMode) {
        self.config.theme = mode;
        self.theme = Theme::for_mode(mode);
        let _ = self.config.save();
    }

    pub fn toggle_theme(&mut self) {
        self.set_theme_mode(self.config.theme.toggle());
    }

    pub fn reset_chat(&mut self) {
        self.stop_streaming();
        self.messages.clear();
        self.block_scroll.clear();
        self.scroll_regions.clear();
        self.composer.clear();
        self.composer_attachments.clear();
        self.attach_status = None;
        self.palette.close();
        self.scroll_spring.snap_to(0.0);
        self.max_scroll = 0.0;
        self.user_scrolled_up = false;
        self.desired_content_height = 0.0;
        self.focus_target = FocusTarget::Composer;
    }

    /// Only explicit summon actions request a new mapping/focus grant.
    pub fn request_focus(&mut self) {
        if !self.has_keyboard_focus {
            self.summon_generation = self.summon_generation.wrapping_add(1);
        }
        self.last_cursor_activity = self.now_secs();
    }

    pub fn show_overlay(&mut self, grab_focus: bool) {
        self.visible = true;
        self.presence_spring.retarget(1.0);
        if grab_focus {
            self.request_focus();
        }
        self.last_cursor_activity = self.now_secs();
    }

    pub fn hide_overlay(&mut self) {
        self.visible = false;
        self.presence_spring.retarget(0.0);
        self.has_keyboard_focus = false;
    }

    pub fn toggle_overlay(&mut self) {
        if self.visible {
            if self.has_keyboard_focus {
                self.hide_overlay();
            } else {
                self.show_overlay(true);
            }
        } else {
            self.show_overlay(true);
        }
    }

    pub fn stop_streaming(&mut self) {
        if let Some(cancel) = self.active_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.active_req_id = None;
        let now = self.now_secs();
        if let Some(last) = self.messages.last_mut() {
            let cur = last.raw_content.clone();
            last.stream.update_raw(&cur, false, now);
            last.streaming = false;
        }
    }

    // ─── Attachments ──────────────────────────────────────────────────────────

    pub fn attach_file_path(&mut self, path: PathBuf) -> bool {
        let now = self.now_secs();
        if !path.exists() {
            self.attach_status = Some((format!("Not found: {}", path.display()), now + 3.0));
            return false;
        }
        if path.is_dir() {
            self.palette.push_route(PaletteRoute::FileBrowser(path));
            return false;
        }

        let (kind, size_bytes) = match validate_before_attach(&self.composer_attachments, &path) {
            Ok(v) => v,
            Err(err) => {
                self.attach_status = Some((err, now + 3.0));
                return false;
            }
        };

        let id = self.next_attach_id;
        self.next_attach_id += 1;
        let mut entry = Attachment::new_loading(id, path.clone(), kind, size_bytes);
        entry.entrance.retarget(1.0);
        self.composer_attachments.push(entry);
        self.attach_status = Some((
            format!(
                "Attached {}",
                path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file")
            ),
            now + 2.2,
        ));
        prepare_attachment_async(id, path, self.attach_tx.clone());
        true
    }

    pub fn attach_raw_image(&mut self, name: String, mime: String, bytes: Vec<u8>) -> bool {
        let now = self.now_secs();
        let id = self.next_attach_id;
        self.next_attach_id += 1;
        let cur_imgs = self
            .composer_attachments
            .iter()
            .filter(|a| a.kind == AttachmentKind::Image)
            .count();
        if self.composer_attachments.len() >= rules::MAX_ATTACHMENTS {
            self.attach_status = Some(("Attachment limit reached".into(), now + 3.0));
            return false;
        }
        if cur_imgs >= rules::MAX_IMAGES {
            self.attach_status = Some((
                format!("Max {} images per message", rules::MAX_IMAGES),
                now + 3.0,
            ));
            return false;
        }
        if (bytes.len() as u64) > rules::MAX_IMAGE_BYTES {
            self.attach_status = Some(("Clipboard image exceeds 16 MiB limit".into(), now + 3.0));
            return false;
        }
        let b64 = crate::base64::encode(&bytes);
        let prep = PreparedAttachment {
            kind: AttachmentKind::Image,
            mime,
            size_bytes: bytes.len() as u64,
            extracted_text: None,
            page_count: None,
            truncated: false,
            base64_data: Some(b64),
            page_images: Vec::new(),
            dimensions: None,
            note: None,
            error: None,
        };
        let mut entry = Attachment::from_prepared(id, PathBuf::from(&name), prep);
        entry.entrance.snap_to(0.0);
        entry.entrance.retarget(1.0);
        self.composer_attachments.push(entry);
        self.attach_status = Some((format!("Pasted {}", name), now + 2.2));
        true
    }

    pub fn try_paste_clipboard_attachments(&mut self) -> bool {
        let now = self.now_secs();
        let clip = self.read_clipboard();
        let trimmed = clip.trim();
        if !trimmed.is_empty() {
            let candidate = expand_tilde(trimmed);
            if candidate.exists() && candidate.is_file() {
                return self.attach_file_path(candidate);
            }
        }
        self.attach_status = Some(("Checking clipboard…".into(), now + 1.5));
        paste_clipboard_attachment_async(self.attach_tx.clone());
        true
    }

    pub fn remove_attachment(&mut self, idx: usize) {
        if idx < self.composer_attachments.len() {
            self.composer_attachments.remove(idx);
        }
    }

    // ─── Composer & Chat Execution ────────────────────────────────────────────

    pub fn submit_composer(&mut self) {
        if self.slash_menu_open() {
            let rows = self.palette_rows();
            let sel = self
                .palette
                .current_frame()
                .map(|f| f.selected)
                .unwrap_or(0);
            if let Some(row) = rows.get(sel).cloned() {
                self.activate_palette_row(row);
            }
            return;
        }

        let raw = self.composer.text.trim().to_string();
        if raw.is_empty() && self.composer_attachments.is_empty() {
            return;
        }

        let now = self.now_secs();
        if self
            .composer_attachments
            .iter()
            .any(|a| a.status == AttachmentStatus::Loading)
        {
            self.attach_status = Some(("Preparing attachment…".into(), now + 2.0));
            return;
        }

        if raw == "/settings" {
            self.composer.clear();
            self.open_settings();
            return;
        }
        if raw == "/attach" {
            self.composer.clear();
            self.palette.open_root();
            self.palette.push_route(PaletteRoute::Attach);
            return;
        }
        if raw == "/provider" {
            self.composer.clear();
            self.palette.open_root();
            self.palette.push_route(PaletteRoute::Provider);
            return;
        }

        let ready_attachments: Vec<AttachmentPayload> = self
            .composer_attachments
            .drain(..)
            .filter(|a| a.status == AttachmentStatus::Ready)
            .map(|a| a.to_payload())
            .collect();

        let prompt_text = if raw.is_empty() && !ready_attachments.is_empty() {
            "Analyze the attached file(s).".to_string()
        } else {
            raw
        };

        self.stop_streaming();
        self.composer.clear();
        self.palette.close();
        self.user_scrolled_up = false;

        self.messages.push(ChatItem::new_user_with_attachments(
            prompt_text,
            ready_attachments,
        ));
        self.start_assistant_completion();
    }

    pub fn regenerate_last(&mut self) {
        self.stop_streaming();
        if matches!(self.messages.last().map(|m| m.role), Some(Role::Assistant)) {
            self.messages.pop();
        }
        if self.messages.iter().any(|m| m.role == Role::User) {
            self.user_scrolled_up = false;
            self.start_assistant_completion();
        }
    }

    fn start_assistant_completion(&mut self) {
        let mut api_msgs: Vec<ChatTurnMsg> = Vec::new();
        for m in &self.messages {
            if m.error.is_some() && m.raw_content.trim().is_empty() {
                continue;
            }
            let content = match m.role {
                Role::User => m.raw_content.clone(),
                Role::Assistant => m.stream.full_answer().trim().to_string(),
            };
            if !content.is_empty() || !m.attachments.is_empty() {
                api_msgs.push(ChatTurnMsg {
                    role: match m.role {
                        Role::User => "user".to_string(),
                        Role::Assistant => "assistant".to_string(),
                    },
                    content,
                    attachments: m.attachments.clone(),
                });
            }
        }

        let now = self.now_secs();
        self.messages.push(ChatItem::new_assistant_streaming(now));

        let req_id = self.next_req_id;
        self.next_req_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.active_req_id = Some(req_id);
        self.active_cancel = Some(cancel.clone());

        spawn_chat_stream(
            req_id,
            self.config.clone(),
            api_msgs,
            cancel,
            self.llm_tx.clone(),
            None,
        );
        self.scroll_spring.retarget(self.max_scroll + 160.0);
    }

    // ─── Palette Actions & Inline Editing ─────────────────────────────────────

    fn clear_slash_trigger_if_needed(&mut self) {
        if self.composer_has_slash_trigger() {
            self.composer.clear();
        }
    }

    fn begin_palette_field_edit(&mut self, field: ProviderField) {
        let cur = match field {
            ProviderField::ProfileName => self.config.active_profile_label().to_string(),
            ProviderField::BaseUrl => self.config.base_url.clone(),
            ProviderField::ApiKey => self.config.api_key.clone(),
        };
        self.palette.edit_buffer.set(cur);
        self.palette.editing_field = Some(field);
        self.focus_target = FocusTarget::PaletteField(field);
    }

    fn commit_palette_field_edit(&mut self) {
        let Some(field) = self.palette.editing_field.take() else {
            return;
        };
        let val = self.palette.edit_buffer.text.trim().to_string();
        let mut name = self.config.active_profile_label().to_string();
        let fmt = self.config.active_format();
        let mut url = self.config.base_url.clone();
        let mut key = self.config.api_key.clone();
        let model = self.config.model.clone();
        let mut should_probe = false;

        match field {
            ProviderField::ProfileName => {
                if !val.is_empty() {
                    name = val;
                }
            }
            ProviderField::BaseUrl => {
                if !val.is_empty() && val != url {
                    url = val;
                    should_probe = true;
                }
            }
            ProviderField::ApiKey => {
                if val != key {
                    key = val;
                    should_probe = true;
                }
            }
        }
        self.config
            .update_active_profile(&name, fmt, &url, &key, &model);
        let _ = self.config.save();
        self.sync_fields_from_config();
        self.focus_target = FocusTarget::Composer;
        if should_probe {
            self.discovered_models.clear();
            self.trigger_model_discovery(false);
        }
    }

    fn open_palette_dropdown(&mut self, kind: DropdownKind) {
        let opts = self.palette.build_dropdown_options(kind, &self.config);
        let active_idx = opts.iter().position(|o| o.is_active).unwrap_or(0);
        self.palette.dropdown = Some(kind);
        self.palette.dropdown_selected = active_idx;
        self.palette.dropdown_scroll = active_idx.saturating_sub(3);
        self.palette_dropdown_highlight_y
            .snap_to((active_idx.saturating_sub(self.palette.dropdown_scroll) as f32) * crate::palette::ROW_STEP);
    }

    fn apply_palette_dropdown_selection(&mut self, idx: usize) {
        let Some(kind) = self.palette.dropdown else {
            return;
        };
        let opts = self.palette.build_dropdown_options(kind, &self.config);
        let Some(opt) = opts.get(idx) else {
            return;
        };
        match kind {
            DropdownKind::Profile => {
                if let Some(_) = profile_from_dropdown_selection(&mut self.config, &opt.id) {
                    let _ = self.config.save();
                    self.sync_fields_from_config();
                    self.discovered_models.clear();
                    self.trigger_model_discovery(false);
                }
            }
            DropdownKind::Protocol => {
                let fmt = if opt.id == "anthropic" {
                    ApiFormat::Anthropic
                } else {
                    ApiFormat::OpenAi
                };
                self.toggle_protocol_format(Some(fmt));
            }
        }
        self.palette.dropdown = None;
    }

    pub fn activate_palette_row(&mut self, row: PaletteRow) {
        let now = self.now_secs();
        match row.action {
            PaletteRowAction::OpenSettings => {
                self.clear_slash_trigger_if_needed();
                self.palette.push_route(PaletteRoute::Settings);
            }
            PaletteRowAction::OpenProvider => {
                self.clear_slash_trigger_if_needed();
                self.palette.push_route(PaletteRoute::Provider);
            }
            PaletteRowAction::OpenModels => {
                self.clear_slash_trigger_if_needed();
                self.palette.push_route(PaletteRoute::Models);
                if self.discovered_models.is_empty() && !self.loading_models {
                    self.trigger_model_discovery(false);
                }
            }
            PaletteRowAction::OpenTheme => {
                self.clear_slash_trigger_if_needed();
                self.palette.push_route(PaletteRoute::Theme);
            }
            PaletteRowAction::OpenAttach => {
                self.clear_slash_trigger_if_needed();
                self.palette.push_route(PaletteRoute::Attach);
            }
            PaletteRowAction::ToggleThinking => {
                self.clear_slash_trigger_if_needed();
                self.palette.close();
                self.toggle_latest_thinking();
            }
            PaletteRowAction::CopyLastReply => {
                self.clear_slash_trigger_if_needed();
                self.palette.close();
                self.copy_latest_reply();
            }
            PaletteRowAction::RegenerateLast => {
                self.clear_slash_trigger_if_needed();
                self.palette.close();
                self.regenerate_last();
            }
            PaletteRowAction::NewChat => {
                self.clear_slash_trigger_if_needed();
                self.reset_chat();
            }
            PaletteRowAction::HideOverlay => {
                self.clear_slash_trigger_if_needed();
                self.palette.close();
                self.hide_overlay();
            }
            PaletteRowAction::QuitApp => {
                self.should_quit = true;
            }
            PaletteRowAction::ToggleVision => {
                self.toggle_vision();
            }
            PaletteRowAction::OpenFullSettingsWindow => {
                self.palette.close();
                self.open_settings();
            }
            PaletteRowAction::ToggleDropdown(kind) => {
                if self.palette.dropdown == Some(kind) {
                    self.palette.dropdown = None;
                } else {
                    self.open_palette_dropdown(kind);
                }
            }
            PaletteRowAction::EditProviderField(field) => {
                self.begin_palette_field_edit(field);
            }
            PaletteRowAction::ToggleKeyMask => {
                self.show_api_key = !self.show_api_key;
            }
            PaletteRowAction::CreateCustomProfile => {
                self.create_custom_profile_and_select();
                self.attach_status = Some(("Created custom profile".into(), now + 2.0));
            }
            PaletteRowAction::DeleteCustomProfile => {
                self.delete_active_custom_profile();
            }
            PaletteRowAction::RefreshModels => {
                self.discovered_models.clear();
                self.trigger_model_discovery(false);
            }
            PaletteRowAction::SelectModel(model_id) => {
                let name = self.config.active_profile_label().to_string();
                let fmt = self.config.active_format();
                let url = self.config.base_url.clone();
                let key = self.config.api_key.clone();
                self.config
                    .update_active_profile(&name, fmt, &url, &key, &model_id);
                let _ = self.config.save();
                self.sync_fields_from_config();
                self.palette.pop_or_close();
            }
            PaletteRowAction::SelectTheme(mode) => {
                self.set_theme_mode(mode);
            }
            PaletteRowAction::PasteFromClipboard => {
                if self.try_paste_clipboard_attachments() {
                    self.palette.close();
                    self.focus_target = FocusTarget::Composer;
                }
            }
            PaletteRowAction::OpenFileBrowser(dir) => {
                self.palette.push_route(PaletteRoute::FileBrowser(dir));
            }
            PaletteRowAction::AttachTypedPath(path) | PaletteRowAction::BrowserPickFile(path) => {
                if self.attach_file_path(path) {
                    self.palette.close();
                    self.focus_target = FocusTarget::Composer;
                }
            }
            PaletteRowAction::BrowserParent(parent) => {
                let _ = parent;
                self.palette.pop_or_close();
            }
            PaletteRowAction::BrowserEnterDir(dir) => {
                self.palette.push_route(PaletteRoute::FileBrowser(dir));
            }
            PaletteRowAction::RemoveAttachment(idx) => {
                self.remove_attachment(idx);
            }
        }
    }

    pub fn toggle_latest_thinking(&mut self) {
        if let Some(m) = self
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.stream.reasoning.as_deref().is_some_and(|s| !s.trim().is_empty()))
        {
            m.thinking_expanded = !m.thinking_expanded;
        }
    }

    pub fn copy_latest_reply(&mut self) {
        let now = self.now_secs();
        if let Some((idx, m)) = self
            .messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, m)| m.role == Role::Assistant && !m.stream.full_answer().trim().is_empty())
        {
            let text = m.stream.full_answer().trim().to_string();
            self.set_clipboard(&text);
            self.copied_badge = Some((idx, usize::MAX, now + 1.6));
            self.attach_status = Some(("Copied reply to clipboard".into(), now + 1.8));
        }
    }

    fn scroll_horizontal_active(&mut self, delta: f32) {
        if let Some(region) = self
            .pointer_pos
            .and_then(|(x, y)| self.scroll_regions.iter().rev().find(|r| r.rect.contains(x, y)))
            .or_else(|| self.scroll_regions.last())
            .cloned()
        {
            let offset = self.block_scroll.entry(region.key).or_default();
            *offset = (*offset + delta).clamp(0.0, region.max_x);
        }
    }

    // ─── Scroll & Pointer Input ───────────────────────────────────────────────

    pub fn scroll_input(&mut self, dx: f32, dy: f32, direct: bool, shift: bool) {
        // When hovering inside an open palette, wheel/trackpad scrolls palette rows
        if self.slash_menu_open() {
            if let (Some((px, py)), Some(prect)) = (self.pointer_pos, self.palette_viewport_rect) {
                if prect.contains(px, py) {
                    let steps = (dy / 24.0).round() as isize;
                    if steps != 0 {
                        if let Some(dd) = self.palette.dropdown {
                            let len = self.palette.build_dropdown_options(dd, &self.config).len();
                            self.palette.scroll_rows(steps, len);
                        } else {
                            let len = self.palette_rows().len();
                            self.palette.scroll_rows(steps, len);
                        }
                    }
                    return;
                }
            }
        }

        let horizontal = if shift && dx.abs() < 0.01 { dy } else { dx };
        if horizontal.abs() > 0.01 {
            if let Some(region) = self.pointer_pos.and_then(|(x, y)| {
                self.scroll_regions
                    .iter()
                    .rev()
                    .find(|r| r.rect.contains(x, y))
            }) {
                let offset = self.block_scroll.entry(region.key).or_default();
                *offset = (*offset + horizontal).clamp(0.0, region.max_x);
                return;
            }
        }
        if dy.abs() > 0.01 && !shift {
            if let Some(region) = self.pointer_pos.and_then(|(x, y)| {
                self.scroll_regions
                    .iter()
                    .rev()
                    .find(|r| r.rect.contains(x, y))
            }) {
                if region.max_x > 0.5 {
                    let offset = self.block_scroll.entry(region.key).or_default();
                    *offset = (*offset + dy).clamp(0.0, region.max_x);
                    return;
                }
            }
        }
        if shift || self.panel() != Panel::Conversation {
            return;
        }
        if direct {
            let next = (self.scroll_spring.target + dy).clamp(0.0, self.max_scroll);
            self.scroll_spring.retarget(next);
            self.user_scrolled_up = next < self.max_scroll - 24.0;
        } else {
            self.scroll_by(dy);
        }
    }

    pub fn scroll_by(&mut self, delta_px: f32) {
        if self.max_scroll <= 0.5 {
            return;
        }
        let next = (self.scroll_spring.target + delta_px).clamp(0.0, self.max_scroll);
        self.scroll_spring.retarget(next);
        self.user_scrolled_up = next < self.max_scroll - 24.0;
    }

    pub fn handle_click(&mut self, button_left: bool, button_right: bool) {
        let now = self.now_secs();
        self.last_cursor_activity = now;

        if button_right {
            if self.settings_open {
                self.save_and_close_settings();
            } else {
                self.open_settings();
            }
            return;
        }
        if !button_left {
            return;
        }

        self.request_focus();
        if !self.settings_open && !self.slash_menu_open() {
            self.focus_target = FocusTarget::Composer;
        }
        let Some(hit) = self.hovered_hit() else {
            if self.palette.dropdown.is_some() {
                self.palette.dropdown = None;
            }
            if self.settings_profile_dropdown {
                self.settings_profile_dropdown = false;
            }
            return;
        };

        match hit {
            HitTarget::Input(target) => {
                self.focus_target = target;
            }
            HitTarget::SendOrStop => {
                if self.is_streaming() {
                    self.stop_streaming();
                } else {
                    self.submit_composer();
                }
            }
            HitTarget::PaletteBack => {
                if self.palette.dropdown.is_some() {
                    self.palette.dropdown = None;
                } else if self.palette.editing_field.is_some() {
                    self.commit_palette_field_edit();
                } else {
                    self.palette.pop_or_close();
                }
            }
            HitTarget::PaletteSearchBox => {
                self.focus_target = FocusTarget::PaletteSearch;
            }
            HitTarget::PaletteRow(idx) => {
                self.commit_palette_field_edit();
                if let Some(frame) = self.palette.current_frame_mut() {
                    frame.selected = idx;
                }
                let rows = self.palette_rows();
                if let Some(row) = rows.get(idx).cloned() {
                    self.activate_palette_row(row);
                }
            }
            HitTarget::PaletteDropdownOption(idx) => {
                self.apply_palette_dropdown_selection(idx);
            }
            HitTarget::ComposerAttachChipRemove(idx) => {
                self.remove_attachment(idx);
            }
            HitTarget::SettingsProfileToggle => {
                self.settings_profile_dropdown = !self.settings_profile_dropdown;
                self.focus_target = FocusTarget::SettingsProfile;
                if self.settings_profile_dropdown {
                    let profiles = self.config.all_profiles();
                    self.settings_profile_selected = profiles
                        .iter()
                        .position(|p| p.id == self.config.active_profile_id)
                        .unwrap_or(0);
                }
            }
            HitTarget::SettingsProfileOption(idx) => {
                self.settings_profile_dropdown = false;
                let profiles = self.config.all_profiles();
                if let Some(p) = profiles.get(idx) {
                    let id = p.id.clone();
                    self.select_profile_by_id(&id);
                }
                self.focus_target = FocusTarget::SettingsProfile;
            }
            HitTarget::SettingsProtocolToggle => {
                self.toggle_protocol_format(None);
                self.focus_target = FocusTarget::SettingsProtocol;
            }
            HitTarget::SettingsVisionToggle => {
                self.toggle_vision();
                self.focus_target = FocusTarget::SettingsVision;
            }
            HitTarget::SettingsNewProfile => {
                self.create_custom_profile_and_select();
                self.focus_target = FocusTarget::SettingsBaseUrl;
            }
            HitTarget::SettingsDeleteProfile => {
                self.delete_active_custom_profile();
                self.focus_target = FocusTarget::SettingsProfile;
            }
            HitTarget::ModelChip(idx) => {
                if let Some(m) = self.discovered_models.get(idx).cloned() {
                    self.field_model.set(m);
                    self.commit_settings_fields();
                }
                self.focus_target = FocusTarget::SettingsModelChip(idx);
            }
            HitTarget::RefreshModels => {
                self.trigger_model_discovery(false);
                self.focus_target = FocusTarget::SettingsRefresh;
            }
            HitTarget::ToggleKeyMask => {
                self.show_api_key = !self.show_api_key;
                if self.settings_open {
                    self.focus_target = FocusTarget::SettingsKeyMask;
                }
            }
            HitTarget::ThemeToggle(mode) => {
                self.set_theme_mode(mode);
                if self.settings_open {
                    self.focus_target = FocusTarget::SettingsTheme(mode);
                }
            }
            HitTarget::SettingsClose => {
                self.save_and_close_settings();
            }
            HitTarget::OpenSettings => {
                self.open_settings();
            }
            HitTarget::CopyCode { msg_idx, block_idx } => {
                if let Some(msg) = self.messages.get(msg_idx) {
                    let visible = msg.stream.shown_text();
                    let doc = crate::markdown::parse_markdown(
                        visible,
                        msg.stream.is_live(),
                        &[],
                        now,
                        &self.theme,
                    );
                    if let Some(crate::markdown::MdBlock::CodeBlock { raw_code, .. }) =
                        doc.blocks.get(block_idx)
                    {
                        self.set_clipboard(raw_code);
                        self.copied_badge = Some((msg_idx, block_idx, now + 1.6));
                    }
                }
            }
            HitTarget::CopyMessage(msg_idx) => {
                if let Some(msg) = self.messages.get(msg_idx) {
                    let text = msg.stream.shown_text().trim().to_string();
                    self.set_clipboard(&text);
                    self.copied_badge = Some((msg_idx, usize::MAX, now + 1.6));
                }
            }
            HitTarget::RegenerateLast => {
                self.regenerate_last();
            }
            HitTarget::ToggleThinking(msg_idx) => {
                if let Some(msg) = self.messages.get_mut(msg_idx) {
                    msg.thinking_expanded = !msg.thinking_expanded;
                }
            }
            HitTarget::LinkUrl { msg_idx, link_idx } => {
                if let Some(msg) = self.messages.get(msg_idx) {
                    let visible = msg.stream.shown_text();
                    let doc = crate::markdown::parse_markdown(
                        visible,
                        msg.stream.is_live(),
                        &[],
                        now,
                        &self.theme,
                    );
                    if let Some(url) = doc.links.get(link_idx) {
                        open_url_nonblocking(url);
                    }
                }
            }
        }
    }

    pub fn set_clipboard(&mut self, text: &str) {
        self.internal_clipboard = text.to_string();
        let owned = text.to_string();
        std::thread::spawn(move || {
            let candidates: &[(&str, &[&str])] = &[
                ("wl-copy", &[]),
                ("xclip", &["-selection", "clipboard"]),
                ("xsel", &["--clipboard", "--input"]),
            ];
            for (bin, args) in candidates {
                if let Ok(mut child) = Command::new(bin)
                    .args(*args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = stdin.write_all(owned.as_bytes());
                    }
                    let _ = child.wait();
                    return;
                }
            }
        });
    }

    pub fn read_clipboard(&self) -> String {
        let candidates: &[(&str, &[&str])] = &[
            ("wl-paste", &["--no-newline"]),
            ("xclip", &["-selection", "clipboard", "-o"]),
            ("xsel", &["--clipboard", "--output"]),
        ];
        for (bin, args) in candidates {
            if let Ok(out) = Command::new(bin)
                .args(*args)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
            {
                if out.status.success() {
                    if let Ok(s) = String::from_utf8(out.stdout) {
                        if !s.is_empty() {
                            return s.replace("\r\n", "\n");
                        }
                    }
                }
            }
        }
        self.internal_clipboard.clone()
    }

    // ─── Keyboard Control ─────────────────────────────────────────────────────

    fn settings_focus_ring(&self) -> Vec<FocusTarget> {
        let mut ring = vec![
            FocusTarget::SettingsProfile,
            FocusTarget::SettingsProtocol,
            FocusTarget::SettingsVision,
            FocusTarget::SettingsTheme(ThemeMode::Dark),
            FocusTarget::SettingsTheme(ThemeMode::Light),
            FocusTarget::SettingsNewProfile,
        ];
        if self.config.active_profile_is_custom() {
            ring.push(FocusTarget::SettingsDeleteProfile);
        }
        ring.extend([
            FocusTarget::SettingsBaseUrl,
            FocusTarget::SettingsApiKey,
            FocusTarget::SettingsKeyMask,
            FocusTarget::SettingsModel,
            FocusTarget::SettingsRefresh,
        ]);
        let chip_count = self.discovered_models.len().min(6);
        for i in 0..chip_count {
            ring.push(FocusTarget::SettingsModelChip(i));
        }
        ring.push(FocusTarget::SettingsDone);
        ring
    }

    fn cycle_settings_focus(&mut self, reverse: bool) {
        let ring = self.settings_focus_ring();
        if ring.is_empty() {
            return;
        }
        let pos = ring.iter().position(|&f| f == self.focus_target);
        let next = match pos {
            Some(i) if reverse => (i + ring.len() - 1) % ring.len(),
            Some(i) => (i + 1) % ring.len(),
            None => 0,
        };
        self.focus_target = ring[next];
    }

    fn activate_settings_focus(&mut self) {
        match self.focus_target {
            FocusTarget::SettingsProfile => {
                self.settings_profile_dropdown = !self.settings_profile_dropdown;
                if self.settings_profile_dropdown {
                    let profiles = self.config.all_profiles();
                    self.settings_profile_selected = profiles
                        .iter()
                        .position(|p| p.id == self.config.active_profile_id)
                        .unwrap_or(0);
                }
            }
            FocusTarget::SettingsProtocol => self.toggle_protocol_format(None),
            FocusTarget::SettingsVision => self.toggle_vision(),
            FocusTarget::SettingsTheme(mode) => self.set_theme_mode(mode),
            FocusTarget::SettingsNewProfile => {
                self.create_custom_profile_and_select();
                self.focus_target = FocusTarget::SettingsBaseUrl;
            }
            FocusTarget::SettingsDeleteProfile => {
                self.delete_active_custom_profile();
                self.focus_target = FocusTarget::SettingsProfile;
            }
            FocusTarget::SettingsKeyMask => self.show_api_key = !self.show_api_key,
            FocusTarget::SettingsRefresh => self.trigger_model_discovery(false),
            FocusTarget::SettingsModelChip(idx) => {
                if let Some(m) = self.discovered_models.get(idx).cloned() {
                    self.field_model.set(m);
                    self.commit_settings_fields();
                }
            }
            FocusTarget::SettingsDone
            | FocusTarget::SettingsBaseUrl
            | FocusTarget::SettingsApiKey
            | FocusTarget::SettingsModel => self.save_and_close_settings(),
            _ => {}
        }
    }

    pub fn handle_key(
        &mut self,
        keysym: u32,
        utf8: Option<&str>,
        ctrl: bool,
        alt: bool,
        shift: bool,
    ) {
        self.last_cursor_activity = self.now_secs();

        // Global Escape invariant: ALWAYS dismiss overlay cleanly regardless of route/draft/history
        if keysym == 0xff1b {
            if self.palette.editing_field.is_some() {
                self.commit_palette_field_edit();
            }
            if self.settings_open {
                self.save_and_close_settings();
            }
            self.hide_overlay();
            return;
        }

        if ctrl {
            match keysym {
                // Ctrl+k -> toggle command palette (preserving composer draft!)
                0x006b | 0x004b => {
                    if self.settings_open {
                        self.save_and_close_settings();
                        self.palette.open_root();
                    } else if self.slash_menu_open() {
                        if self.composer_has_slash_trigger() {
                            self.composer.clear();
                        }
                        self.palette.close();
                        self.focus_target = FocusTarget::Composer;
                    } else {
                        self.palette.open_root();
                    }
                    return;
                }
                // Ctrl+u -> open /attach palette directly
                0x0075 | 0x0055 => {
                    if self.settings_open {
                        self.save_and_close_settings();
                    }
                    self.palette.open_root();
                    self.palette.push_route(PaletteRoute::Attach);
                    return;
                }
                // Ctrl+, -> toggle full Settings panel
                0x002c => {
                    if self.settings_open {
                        self.save_and_close_settings();
                    } else {
                        self.open_settings();
                    }
                    return;
                }
                // Ctrl+t -> toggle latest reasoning disclosure
                0x0074 | 0x0054 => {
                    self.toggle_latest_thinking();
                    return;
                }
                // Ctrl+r -> refresh models inside palette/settings, or retry last answer in chat
                0x0072 | 0x0052 => {
                    if self.settings_open
                        || matches!(
                            self.palette.current_route(),
                            Some(PaletteRoute::Models | PaletteRoute::Provider | PaletteRoute::Settings)
                        )
                    {
                        self.discovered_models.clear();
                        self.trigger_model_discovery(false);
                    } else {
                        self.regenerate_last();
                    }
                    return;
                }
                // Ctrl+h -> toggle API key masking when editing/viewing provider/settings
                0x0068 | 0x0048 => {
                    if self.settings_open
                        || matches!(self.palette.current_route(), Some(PaletteRoute::Provider))
                    {
                        self.show_api_key = !self.show_api_key;
                        return;
                    }
                }
                // Ctrl+n / Ctrl+l -> reset to thin bar
                0x006e | 0x004e | 0x006c | 0x004c => {
                    if !self.settings_open {
                        self.reset_chat();
                        return;
                    }
                }
                // Ctrl+q -> quit
                0x0071 | 0x0051 => {
                    self.should_quit = true;
                    return;
                }
                // Ctrl+a -> select all in active text field
                0x0061 | 0x0041 => {
                    if self.slash_menu_open()
                        && matches!(self.palette.current_route(), Some(PaletteRoute::Attach))
                        && shift
                    {
                        return;
                    }
                    self.active_field_mut().select_all();
                    return;
                }
                // Ctrl+Shift+C -> copy latest assistant reply; Ctrl+C -> copy selection or stop stream
                0x0063 | 0x0043 => {
                    if shift {
                        self.copy_latest_reply();
                        return;
                    }
                    if let Some(sel) = self.active_field_mut().selected_text().map(str::to_string) {
                        self.set_clipboard(&sel);
                    } else if self.is_streaming() {
                        self.stop_streaming();
                    }
                    return;
                }
                // Ctrl+x -> cut selection
                0x0078 | 0x0058 => {
                    if let Some(sel) = self.active_field_mut().selected_text().map(str::to_string) {
                        self.active_field_mut().delete_selection();
                        self.set_clipboard(&sel);
                    }
                    return;
                }
                // Ctrl+v -> paste image/file if clipboard has one & in composer/attach, else paste text
                0x0076 | 0x0056 => {
                    if matches!(self.palette.current_route(), Some(PaletteRoute::Attach)) {
                        self.try_paste_clipboard_attachments();
                        return;
                    }
                    let clip = self.read_clipboard();
                    if self.panel() != Panel::Settings
                        && self.palette.editing_field.is_none()
                        && !self.palette.has_search_bar()
                    {
                        let trimmed = clip.trim();
                        if trimmed.is_empty()
                            || crate::attach::parse_uri_or_path(trimmed).is_some()
                        {
                            self.try_paste_clipboard_attachments();
                            return;
                        }
                    }
                    if !clip.is_empty() {
                        let multiline =
                            !self.settings_open && !self.slash_menu_open() && self.focus_target == FocusTarget::Composer;
                        let cleaned = if multiline { clip } else { clip.replace('\n', "") };
                        self.active_field_mut().insert_str(&cleaned);
                    }
                    return;
                }
                // Ctrl+w or Ctrl+BackSpace -> delete word back
                0x0077 | 0x0057 | 0xff08 => {
                    self.active_field_mut().delete_word_back();
                    return;
                }
                _ => {}
            }
        }

        // Route-specific full keyboard control
        if self.settings_open {
            self.handle_settings_key(keysym, utf8, ctrl, alt, shift);
            return;
        }

        if self.slash_menu_open() {
            self.handle_palette_key(keysym, utf8, ctrl, alt, shift);
            return;
        }

        // Conversation / Compact keyboard handling
        match keysym {
            // Return / KP_Enter
            0xff0d | 0xff8d => {
                if shift {
                    self.composer.insert_str("\n");
                } else {
                    self.submit_composer();
                }
            }
            // Tab -> open palette if composer starts with `/`, else no-op
            0xff09 => {
                if self.composer_has_slash_trigger() {
                    self.palette.open_root();
                }
            }
            // Up / Down
            0xff52 => {
                if alt || ctrl {
                    self.scroll_by(-72.0);
                } else {
                    self.composer.move_home(shift);
                }
            }
            0xff54 => {
                if alt || ctrl {
                    self.scroll_by(72.0);
                } else {
                    self.composer.move_end(shift);
                }
            }
            // Page_Up / Page_Down
            0xff55 => self.scroll_by(-240.0),
            0xff56 => self.scroll_by(240.0),
            // Left / Right
            0xff51 => {
                if alt {
                    self.scroll_horizontal_active(-96.0);
                } else if ctrl {
                    self.composer.move_word_left(shift);
                } else {
                    self.composer.move_left(shift);
                }
            }
            0xff53 => {
                if alt {
                    self.scroll_horizontal_active(96.0);
                } else if ctrl {
                    self.composer.move_word_right(shift);
                } else {
                    self.composer.move_right(shift);
                }
            }
            0xff50 => self.composer.move_home(shift),
            0xff57 => self.composer.move_end(shift),
            // BackSpace: remove trailing attachment chip if composer text is empty
            0xff08 => {
                if self.composer.text.is_empty() && !self.composer_attachments.is_empty() {
                    self.composer_attachments.pop();
                } else {
                    self.composer.backspace();
                }
            }
            0xffff => self.composer.delete_forward(),
            _ => {
                if !ctrl && !alt {
                    if let Some(txt) = utf8 {
                        let filtered: String = txt
                            .chars()
                            .filter(|c| !c.is_control() || *c == '\t')
                            .collect();
                        if !filtered.is_empty() {
                            self.composer.insert_str(&filtered);
                        }
                    }
                }
            }
        }
    }

    fn handle_settings_key(
        &mut self,
        keysym: u32,
        utf8: Option<&str>,
        ctrl: bool,
        alt: bool,
        shift: bool,
    ) {
        if self.settings_profile_dropdown {
            let profiles = self.config.all_profiles();
            match keysym {
                0xff52 => {
                    if !profiles.is_empty() {
                        self.settings_profile_selected =
                            (self.settings_profile_selected + profiles.len() - 1) % profiles.len();
                    }
                    return;
                }
                0xff54 => {
                    if !profiles.is_empty() {
                        self.settings_profile_selected =
                            (self.settings_profile_selected + 1) % profiles.len();
                    }
                    return;
                }
                0xff0d | 0xff8d | 0x0020 => {
                    if let Some(p) = profiles.get(self.settings_profile_selected) {
                        let id = p.id.clone();
                        self.select_profile_by_id(&id);
                    }
                    self.settings_profile_dropdown = false;
                    return;
                }
                0xff51 | 0xff08 | 0xff09 | 0xfe20 => {
                    self.settings_profile_dropdown = false;
                    return;
                }
                _ => {}
            }
        }

        match keysym {
            // Return / KP_Enter
            0xff0d | 0xff8d => {
                self.activate_settings_focus();
            }
            // Space toggles non-text buttons/dropdowns/chips
            0x0020 if !self.focus_target.is_text_input() => {
                self.activate_settings_focus();
            }
            // Tab / ISO_Left_Tab cycles ALL settings controls
            0xff09 | 0xfe20 => {
                let reverse = shift || keysym == 0xfe20;
                self.cycle_settings_focus(reverse);
            }
            // Up / Down navigate controls cleanly
            0xff52 => self.cycle_settings_focus(true),
            0xff54 => self.cycle_settings_focus(false),
            // Left / Right move cursor inside text fields OR cycle controls when on pills
            0xff51 => {
                if self.focus_target.is_text_input() {
                    if ctrl {
                        self.active_field_mut().move_word_left(shift);
                    } else {
                        self.active_field_mut().move_left(shift);
                    }
                } else {
                    self.cycle_settings_focus(true);
                }
            }
            0xff53 => {
                if self.focus_target.is_text_input() {
                    if ctrl {
                        self.active_field_mut().move_word_right(shift);
                    } else {
                        self.active_field_mut().move_right(shift);
                    }
                } else {
                    self.cycle_settings_focus(false);
                }
            }
            0xff50 if self.focus_target.is_text_input() => {
                self.active_field_mut().move_home(shift);
            }
            0xff57 if self.focus_target.is_text_input() => {
                self.active_field_mut().move_end(shift);
            }
            0xff08 if self.focus_target.is_text_input() => {
                self.active_field_mut().backspace();
            }
            0xffff if self.focus_target.is_text_input() => {
                self.active_field_mut().delete_forward();
            }
            _ => {
                if !ctrl && !alt && self.focus_target.is_text_input() {
                    if let Some(txt) = utf8 {
                        let filtered: String = txt
                            .chars()
                            .filter(|c| !c.is_control() || *c == '\t')
                            .collect();
                        if !filtered.is_empty() {
                            self.active_field_mut().insert_str(&filtered);
                        }
                    }
                }
            }
        }
    }

    fn handle_palette_key(
        &mut self,
        keysym: u32,
        utf8: Option<&str>,
        ctrl: bool,
        alt: bool,
        shift: bool,
    ) {
        if self.palette.stack.is_empty() {
            self.palette
                .stack
                .push(crate::palette::PaletteFrame::new(PaletteRoute::Root));
        }

        // 1. Active dropdown inside palette (Profile or Protocol)
        if let Some(dd) = self.palette.dropdown {
            let opts = self.palette.build_dropdown_options(dd, &self.config);
            let len = opts.len();
            match keysym {
                0xff52 => self.palette.move_dropdown_selection(-1, len),
                0xff54 => self.palette.move_dropdown_selection(1, len),
                0xff55 => self.palette.move_dropdown_selection(-5, len),
                0xff56 => self.palette.move_dropdown_selection(5, len),
                0xff0d | 0xff8d | 0x0020 => {
                    let sel = self.palette.dropdown_selected;
                    self.apply_palette_dropdown_selection(sel);
                }
                0xff51 | 0xff08 | 0xff09 | 0xfe20 => {
                    self.palette.dropdown = None;
                }
                _ => {}
            }
            return;
        }

        // 2. Active inline TextField editing inside Provider submenu
        if self.palette.editing_field.is_some() {
            match keysym {
                0xff0d | 0xff8d => {
                    self.commit_palette_field_edit();
                }
                0xff09 | 0xfe20 => {
                    let reverse = shift || keysym == 0xfe20;
                    self.commit_palette_field_edit();
                    let rows = self.palette_rows();
                    self.palette
                        .move_selection(if reverse { -1 } else { 1 }, rows.len());
                    let sel = self
                        .palette
                        .current_frame()
                        .map(|f| f.selected)
                        .unwrap_or(0);
                    if let Some(row) = rows.get(sel) {
                        if let Some(next_field) = row.editable {
                            self.begin_palette_field_edit(next_field);
                        }
                    }
                }
                0xff52 => {
                    self.commit_palette_field_edit();
                    let rows = self.palette_rows();
                    self.palette.move_selection(-1, rows.len());
                }
                0xff54 => {
                    self.commit_palette_field_edit();
                    let rows = self.palette_rows();
                    self.palette.move_selection(1, rows.len());
                }
                0xff51 => {
                    if ctrl {
                        self.palette.edit_buffer.move_word_left(shift);
                    } else {
                        self.palette.edit_buffer.move_left(shift);
                    }
                }
                0xff53 => {
                    if ctrl {
                        self.palette.edit_buffer.move_word_right(shift);
                    } else {
                        self.palette.edit_buffer.move_right(shift);
                    }
                }
                0xff50 => self.palette.edit_buffer.move_home(shift),
                0xff57 => self.palette.edit_buffer.move_end(shift),
                0xff08 => self.palette.edit_buffer.backspace(),
                0xffff => self.palette.edit_buffer.delete_forward(),
                _ => {
                    if !ctrl && !alt {
                        if let Some(txt) = utf8 {
                            let filtered: String = txt
                                .chars()
                                .filter(|c| !c.is_control() || *c == '\t')
                                .collect();
                            if !filtered.is_empty() {
                                self.palette.edit_buffer.insert_str(&filtered);
                            }
                        }
                    }
                }
            }
            return;
        }

        // 3. Normal palette list / filter navigation
        let rows = self.palette_rows();
        let row_len = rows.len();
        let sel = self
            .palette
            .current_frame()
            .map(|f| f.selected.min(row_len.saturating_sub(1)))
            .unwrap_or(0);

        let using_composer_slash =
            !self.palette.open && self.composer_has_slash_trigger();
        let filter_empty = if using_composer_slash {
            self.composer.text.trim() == "/"
        } else {
            self.palette
                .current_frame()
                .map(|f| f.query.text.is_empty())
                .unwrap_or(true)
        };

        match keysym {
            // Return / KP_Enter
            0xff0d | 0xff8d => {
                if let Some(row) = rows.get(sel).cloned() {
                    self.activate_palette_row(row);
                }
            }
            // Tab / Shift+Tab -> Shift+Tab backs up one submenu; Tab drills into submenu/dropdown/field or completes
            0xff09 | 0xfe20 => {
                if shift || keysym == 0xfe20 {
                    if !self.palette.pop_or_close() && using_composer_slash {
                        self.composer.clear();
                    }
                } else if let Some(row) = rows.get(sel).cloned() {
                    if row.is_submenu || row.is_dropdown || row.editable.is_some() {
                        self.activate_palette_row(row);
                    } else if using_composer_slash && row.title.starts_with('/') {
                        self.composer.set(&row.title);
                    }
                }
            }
            // Up / Down / PageUp / PageDown
            0xff52 => self.palette.move_selection(-1, row_len),
            0xff54 => self.palette.move_selection(1, row_len),
            0xff55 => self.palette.move_selection(-5, row_len),
            0xff56 => self.palette.move_selection(5, row_len),
            // Right -> drill into submenu/dropdown/editable field when filter cursor at end (or no filter)
            0xff53 => {
                if filter_empty || matches!(self.palette.current_route(), Some(PaletteRoute::Provider | PaletteRoute::Theme)) {
                    if let Some(row) = rows.get(sel).cloned() {
                        if row.is_submenu || row.is_dropdown || row.editable.is_some() {
                            self.activate_palette_row(row);
                            return;
                        }
                    }
                }
                self.active_field_mut().move_right(shift);
            }
            // Left -> back out one submenu if filter is empty (or in Provider/Theme), else move cursor left
            0xff51 => {
                if filter_empty
                    || matches!(
                        self.palette.current_route(),
                        Some(PaletteRoute::Provider | PaletteRoute::Theme)
                    )
                {
                    if self.palette.pop_or_close() {
                        return;
                    }
                    if using_composer_slash {
                        self.composer.clear();
                    }
                    return;
                }
                self.active_field_mut().move_left(shift);
            }
            0xff50 => self.active_field_mut().move_home(shift),
            0xff57 => self.active_field_mut().move_end(shift),
            // BackSpace -> delete query/composer char, or pop submenu when query is empty
            0xff08 => {
                if using_composer_slash {
                    self.composer.backspace();
                    if !self.composer_has_slash_trigger() {
                        self.palette.close();
                    }
                } else if filter_empty {
                    self.palette.pop_or_close();
                } else {
                    self.active_field_mut().backspace();
                }
                if let Some(frame) = self.palette.current_frame_mut() {
                    frame.selected = 0;
                    frame.scroll_row = 0;
                }
            }
            0xffff => self.active_field_mut().delete_forward(),
            // Space toggles boolean/dropdown/action rows when not filtering text
            0x0020
                if matches!(
                    self.palette.current_route(),
                    Some(PaletteRoute::Provider | PaletteRoute::Theme)
                ) =>
            {
                if let Some(row) = rows.get(sel).cloned() {
                    self.activate_palette_row(row);
                }
            }
            _ => {
                if !ctrl && !alt {
                    if let Some(txt) = utf8 {
                        let filtered: String = txt
                            .chars()
                            .filter(|c| !c.is_control() || *c == '\t')
                            .collect();
                        if !filtered.is_empty() {
                            // Typing straight onto an inline editable row in Provider enters edit mode automatically!
                            if matches!(self.palette.current_route(), Some(PaletteRoute::Provider)) {
                                if let Some(row) = rows.get(sel) {
                                    if let Some(field) = row.editable {
                                        self.begin_palette_field_edit(field);
                                        self.palette.edit_buffer.insert_str(&filtered);
                                        return;
                                    }
                                }
                            }
                            if using_composer_slash {
                                self.composer.insert_str(&filtered);
                            } else {
                                if self.palette.stack.is_empty() {
                                    self.palette.open_root();
                                }
                                if let Some(frame) = self.palette.current_frame_mut() {
                                    frame.query.insert_str(&filtered);
                                    frame.selected = 0;
                                    frame.scroll_row = 0;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod interaction_tests {
    use super::*;
    fn app() -> AppState {
        let mut config = AppConfig::default();
        config.model = "test-model".into();
        AppState::new(config)
    }

    #[test]
    fn panel_priority_is_shared() {
        let mut app = app();
        assert_eq!(app.panel(), Panel::Compact);
        app.messages.push(ChatItem::new_user("hello".to_string()));
        assert_eq!(app.panel(), Panel::Conversation);
        app.composer.set("/");
        assert_eq!(app.panel(), Panel::Commands);
        app.settings_open = true;
        assert_eq!(app.panel(), Panel::Settings);
    }

    #[test]
    fn escape_dismisses_regardless_of_draft_or_history() {
        for history in [false, true] {
            for draft in ["", "draft", "/"] {
                let mut app = app();
                if history {
                    app.messages.push(ChatItem::new_user("hello".to_string()));
                }
                app.composer.set(draft);
                app.handle_key(0xff1b, None, false, false, false);
                assert!(!app.visible);
                assert!(!app.wants_keyboard_on_demand());
            }
        }
    }

    #[test]
    fn eligibility_is_independent_of_pointer_and_ownership() {
        let mut app = app();
        for focused in [false, true] {
            app.has_keyboard_focus = focused;
            for pointer in [None, Some((0.0, 0.0)), Some((300.0, 300.0))] {
                app.pointer_pos = pointer;
                assert!(app.wants_keyboard_on_demand());
            }
        }
        let generation = app.summon_generation;
        app.has_keyboard_focus = false;
        app.handle_click(true, false);
        assert_eq!(app.focus_target, FocusTarget::Composer);
        assert_eq!(app.summon_generation, generation + 1);
        app.show_overlay(true);
        assert_eq!(app.summon_generation, generation + 2);
    }

    #[test]
    fn ctrl_k_preserves_composer_draft_and_navigates_nested_submenus() {
        let mut app = app();
        app.composer.set("Summarize my architecture notes");
        // Ctrl+k opens palette without erasing draft
        app.handle_key(0x006b, Some("k"), true, false, false);
        assert_eq!(app.panel(), Panel::Commands);
        assert_eq!(app.composer.text, "Summarize my architecture notes");

        // Down -> /settings -> Right/Enter drills into Settings -> Enter drills into Provider
        app.handle_key(0xff54, None, false, false, false);
        assert_eq!(app.palette.current_frame().unwrap().selected, 1);
        app.handle_key(0xff0d, None, false, false, false);
        assert_eq!(app.palette.current_route(), Some(&PaletteRoute::Settings));
        app.handle_key(0xff0d, None, false, false, false);
        assert_eq!(app.palette.current_route(), Some(&PaletteRoute::Provider));

        // Open Profile dropdown
        app.handle_key(0xff0d, None, false, false, false);
        assert_eq!(app.palette.dropdown, Some(DropdownKind::Profile));

        // Left closes dropdown -> Left backs to Settings -> Left backs to Root -> Left closes palette
        app.handle_key(0xff51, None, false, false, false);
        assert_eq!(app.palette.dropdown, None);
        app.handle_key(0xff51, None, false, false, false);
        assert_eq!(app.palette.current_route(), Some(&PaletteRoute::Settings));
        app.handle_key(0xff51, None, false, false, false);
        assert_eq!(app.palette.current_route(), Some(&PaletteRoute::Root));
        app.handle_key(0xff51, None, false, false, false);
        assert!(!app.slash_menu_open());
        assert_eq!(app.composer.text, "Summarize my architecture notes");
    }

    #[test]
    fn settings_panel_keyboard_cycles_all_controls_and_custom_profiles() {
        let mut app = app();
        app.open_settings();
        assert_eq!(app.focus_target, FocusTarget::SettingsBaseUrl);
        app.handle_key(0xff09, None, false, false, false);
        assert_eq!(app.focus_target, FocusTarget::SettingsApiKey);
        app.handle_key(0xff09, None, false, false, false);
        assert_eq!(app.focus_target, FocusTarget::SettingsKeyMask);
        app.handle_key(0x0020, Some(" "), false, false, false);
        assert!(app.show_api_key);
    }
}

fn open_url_nonblocking(url: &str) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    let u = url.to_string();
    std::thread::spawn(move || {
        let _ = Command::new("xdg-open")
            .arg(&u)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}
