use crate::anim::{LogicalRect, RectSpring, Spring, SpringSpec};
use crate::config::{AppConfig, ProviderPreset, PRESETS};
use crate::llm::{fetch_models_async, spawn_chat_stream, ChatTurnMsg, LlmEvent};
use crate::stream::SmoothStream;
use crate::theme::{Theme, ThemeMode};
use std::io::Write;
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
    SettingsBaseUrl,
    SettingsApiKey,
    SettingsModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    Input(FocusTarget),
    SendOrStop,
    SlashItem(usize),
    PresetPill(usize),
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
    pub stream: SmoothStream,
    pub streaming: bool,
    pub error: Option<String>,
    pub thinking_expanded: bool,
    pub thinking_reveal: Spring,
    pub entrance: Spring,
}

impl ChatItem {
    pub fn new_user(text: String) -> Self {
        let s = SmoothStream::from_completed(&text);
        let mut entrance = Spring::new(0.0, SpringSpec::FAST_SPATIAL);
        entrance.retarget(1.0);
        Self {
            role: Role::User,
            raw_content: text,
            stream: s,
            streaming: false,
            error: None,
            thinking_expanded: false,
            thinking_reveal: Spring::new(0.0, SpringSpec::SCROLL),
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
            stream: s,
            streaming: true,
            error: None,
            thinking_expanded: false,
            thinking_reveal: Spring::new(0.0, SpringSpec::SCROLL),
            entrance,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashEntry {
    pub cmd: &'static str,
    pub label: &'static str,
}

pub const SLASH_COMMANDS: &[SlashEntry] = &[
    SlashEntry {
        cmd: "/settings",
        label: "Endpoint, optional API key & model",
    },
    SlashEntry {
        cmd: "/new",
        label: "Reset to thin bar",
    },
    SlashEntry {
        cmd: "/theme",
        label: "Switch Quiet Paper dark / light",
    },
    SlashEntry {
        cmd: "/hide",
        label: "Dismiss overlay",
    },
    SlashEntry {
        cmd: "/quit",
        label: "Exit thetalk",
    },
];

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

    // Slash menu
    pub slash_selected: usize,

    // Settings state
    pub settings_open: bool,
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
            slash_selected: 0,
            settings_open: false,
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

    pub fn filtered_slash_commands(&self) -> Vec<(usize, &'static SlashEntry)> {
        let trimmed = self.composer.text.trim_start();
        if !trimmed.starts_with('/') || trimmed.contains(' ') || trimmed.contains('\n') {
            return Vec::new();
        }
        let q = trimmed.to_ascii_lowercase();
        SLASH_COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.cmd.starts_with(&q))
            .collect()
    }

    pub fn slash_menu_open(&self) -> bool {
        !self.settings_open && !self.filtered_slash_commands().is_empty()
    }

    pub fn panel(&self) -> Panel {
        if self.settings_open { Panel::Settings }
        else if self.slash_menu_open() { Panel::Commands }
        else if !self.messages.is_empty() { Panel::Conversation }
        else { Panel::Compact }
    }

    pub fn is_expanded(&self) -> bool {
        self.panel() != Panel::Compact
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
        let composer_h = self.desired_composer_height.clamp(BAR_MIN_HEIGHT, 168.0);
        let max_above = CARD_MAX_HEIGHT - composer_h;
        let above_h = self.desired_content_height.clamp(0.0, max_above);
        let target = Self::compute_target_rect(expanded, above_h, composer_h);
        self.card_rect.retarget(target);
        self.card_radius.retarget(if expanded { 16.0 } else { 23.0 });

        let show_send = !self.composer.text.trim().is_empty() || self.is_streaming();
        self.send_bloom.retarget(if show_send { 1.0 } else { 0.0 });

    }

    /// Advances all physics springs, streaming token revealers, and background LLM events.
    /// Returns `true` if another animation frame should be scheduled immediately.
    pub fn tick(&mut self, dt: f32) -> bool {
        let now = self.now_secs();

        // Poll LLM events (`ModelsFetched`, `ModelAutoSelected`, `Delta`, `Finished`, `Error`)
        // A fast producer must not monopolize the UI thread indefinitely.
        for _ in 0..128 {
            let Ok(ev) = self.llm_rx.try_recv() else { break; };
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
                            if self.settings_open {
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
            msg.thinking_reveal.retarget(if msg.thinking_expanded { 1.0 } else { 0.0 });
            if msg.thinking_reveal.advance(dt) { any_stream_anim = true; }
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

        self.sync_geometry_targets();

        let mut moving = false;
        moving |= self.presence_spring.advance(dt);
        moving |= self.card_rect.advance(dt);
        moving |= self.card_radius.advance(dt);
        moving |= self.scroll_spring.advance(dt);
        moving |= self.send_bloom.advance(dt);


        if !self.visible
            && !self.presence_spring.is_running()
            && self.presence_spring.value <= 0.02
        {
            return false;
        }

        moving || any_stream_anim || self.loading_models || now < self.panel_transition_until
    }

    /// Eligibility is not ownership. niri grants OnDemand focus on mapping or
    /// click, and revokes it when another surface is clicked. Never toggle this
    /// based on pointer hover, stream events, or individual widget hitboxes.
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
        match self.focus_target {
            FocusTarget::Composer => &mut self.composer,
            FocusTarget::SettingsBaseUrl => &mut self.field_base_url,
            FocusTarget::SettingsApiKey => &mut self.field_api_key,
            FocusTarget::SettingsModel => &mut self.field_model,
        }
    }

    pub fn open_settings(&mut self) {
        self.settings_open = true;
        self.field_base_url.set(self.config.base_url.clone());
        self.field_api_key.set(self.config.api_key.clone());
        self.field_model.set(self.config.model.clone());
        self.focus_target = FocusTarget::SettingsBaseUrl;
        self.request_focus();
        self.trigger_model_discovery(false);
    }

    pub fn save_and_close_settings(&mut self) {
        self.config.base_url = self.field_base_url.text.trim().to_string();
        if self.config.base_url.is_empty() {
            self.config.base_url = "http://localhost:11434/v1".to_string();
        }
        self.config.api_key = self.field_api_key.text.trim().to_string();
        self.config.model = self.field_model.text.trim().to_string();
        let _ = self.config.save();
        self.settings_open = false;
        self.focus_target = FocusTarget::Composer;
        self.last_cursor_activity = self.now_secs();
    }

    pub fn apply_preset(&mut self, idx: usize) {
        if let Some(ProviderPreset {
            base_url,
            default_model,
            ..
        }) = PRESETS.get(idx)
        {
            self.field_base_url.set(*base_url);
            if self.field_model.text.trim().is_empty() && !default_model.is_empty() {
                self.field_model.set(*default_model);
            }
            self.config.base_url = (*base_url).to_string();
            self.trigger_model_discovery(false);
        }
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
                // Overlay was passively visible while working in another window — summon focus to bar!
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

    pub fn submit_composer(&mut self) {
        let raw = self.composer.text.trim().to_string();
        if raw.is_empty() {
            return;
        }

        if self.slash_menu_open() {
            let cmds = self.filtered_slash_commands();
            let pick = self.slash_selected.min(cmds.len().saturating_sub(1));
            if let Some((_, entry)) = cmds.get(pick) {
                self.composer.clear();
                self.slash_selected = 0;
                self.execute_slash_command(entry.cmd);
                return;
            }
        }

        if raw == "/settings" {
            self.composer.clear();
            self.open_settings();
            return;
        }

        self.stop_streaming();
        self.composer.clear();
        self.slash_selected = 0;
        self.user_scrolled_up = false;

        self.messages.push(ChatItem::new_user(raw));
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
            if !content.is_empty() {
                api_msgs.push(ChatTurnMsg {
                    role: match m.role {
                        Role::User => "user".to_string(),
                        Role::Assistant => "assistant".to_string(),
                    },
                    content,
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

    pub fn execute_slash_command(&mut self, cmd: &str) {
        match cmd {
            "/settings" => self.open_settings(),
            "/new" => self.reset_chat(),
            "/theme" => self.toggle_theme(),
            "/hide" => self.hide_overlay(),
            "/quit" => self.should_quit = true,
            _ => {}
        }
    }

    pub fn scroll_input(&mut self, dx: f32, dy: f32, direct: bool, shift: bool) {
        let horizontal = if shift && dx.abs() < 0.01 { dy } else { dx };
        if horizontal.abs() > 0.01 {
            if let Some(region) = self.pointer_pos.and_then(|(x, y)|
                self.scroll_regions.iter().rev().find(|r| r.rect.contains(x, y))) {
                let offset = self.block_scroll.entry(region.key).or_default();
                *offset = (*offset + horizontal).clamp(0.0, region.max_x);
                return;
            }
        }
        if shift || self.panel() != Panel::Conversation { return; }
        if direct {
            let next = (self.scroll_spring.value + dy).clamp(0.0, self.max_scroll);
            self.scroll_spring.snap_to(next);
            self.user_scrolled_up = next < self.max_scroll - 24.0;
        } else { self.scroll_by(dy); }
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

        // Explicit card click is a focus request too. Some compositor/shell
        // combinations do not deliver keyboard enter for blank layer content.
        self.request_focus();
        if !self.settings_open {
            self.focus_target = FocusTarget::Composer;
        }
        let Some(hit) = self.hovered_hit() else {
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
            HitTarget::SlashItem(idx) => {
                let cmds = self.filtered_slash_commands();
                if let Some((_, entry)) = cmds.get(idx).copied() {
                    self.composer.clear();
                    self.slash_selected = 0;
                    self.execute_slash_command(entry.cmd);
                }
            }
            HitTarget::PresetPill(idx) => {
                self.apply_preset(idx);
            }
            HitTarget::ModelChip(idx) => {
                if let Some(m) = self.discovered_models.get(idx).cloned() {
                    self.field_model.set(m.clone());
                    self.config.model = m;
                    let _ = self.config.save();
                }
            }
            HitTarget::RefreshModels => {
                self.trigger_model_discovery(false);
            }
            HitTarget::ToggleKeyMask => {
                self.show_api_key = !self.show_api_key;
            }
            HitTarget::ThemeToggle(mode) => {
                self.set_theme_mode(mode);
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

    pub fn handle_key(
        &mut self,
        keysym: u32,
        utf8: Option<&str>,
        ctrl: bool,
        alt: bool,
        shift: bool,
    ) {
        self.last_cursor_activity = self.now_secs();

        if ctrl {
            match keysym {
                // Ctrl+, -> toggle Settings
                0x002c => {
                    if self.settings_open {
                        self.save_and_close_settings();
                    } else {
                        self.open_settings();
                    }
                    return;
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
                // Ctrl+a -> select all
                0x0061 | 0x0041 => {
                    self.active_field_mut().select_all();
                    return;
                }
                // Ctrl+c -> copy selection or stop stream if nothing selected
                0x0063 | 0x0043 => {
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
                // Ctrl+v -> paste
                0x0076 | 0x0056 => {
                    let clip = self.read_clipboard();
                    if !clip.is_empty() {
                        let cleaned = if self.focus_target == FocusTarget::Composer {
                            clip
                        } else {
                            clip.replace('\n', "")
                        };
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

        match keysym {
            // Escape always dismisses the overlay, independent of route,
            // conversation history, draft content, or streaming state.
            0xff1b => {
                if self.settings_open {
                    self.save_and_close_settings();
                }
                self.hide_overlay();
            }
            // Return / KP_Enter
            0xff0d | 0xff8d => {
                if self.settings_open {
                    self.save_and_close_settings();
                } else if shift && !self.slash_menu_open() {
                    self.composer.insert_str("\n");
                } else {
                    self.submit_composer();
                }
            }
            // Tab / ISO_Left_Tab
            0xff09 | 0xfe20 => {
                if self.settings_open {
                    let reverse = shift || keysym == 0xfe20;
                    self.focus_target = match (self.focus_target, reverse) {
                        (FocusTarget::SettingsBaseUrl, false) => FocusTarget::SettingsApiKey,
                        (FocusTarget::SettingsApiKey, false) => FocusTarget::SettingsModel,
                        (FocusTarget::SettingsModel, false) => FocusTarget::SettingsBaseUrl,
                        (FocusTarget::SettingsBaseUrl, true) => FocusTarget::SettingsModel,
                        (FocusTarget::SettingsApiKey, true) => FocusTarget::SettingsBaseUrl,
                        (FocusTarget::SettingsModel, true) => FocusTarget::SettingsApiKey,
                        _ => FocusTarget::SettingsBaseUrl,
                    };
                } else if self.slash_menu_open() {
                    let cmds = self.filtered_slash_commands();
                    let pick = self.slash_selected.min(cmds.len().saturating_sub(1));
                    if let Some((_, entry)) = cmds.get(pick) {
                        self.composer.set(entry.cmd);
                    }
                }
            }
            // Up
            0xff52 => {
                if self.slash_menu_open() {
                    let len = self.filtered_slash_commands().len();
                    if len > 0 {
                        self.slash_selected = (self.slash_selected + len - 1) % len;
                    }
                } else if alt || ctrl {
                    self.scroll_by(-72.0);
                } else {
                    self.active_field_mut().move_home(shift);
                }
            }
            // Down
            0xff54 => {
                if self.slash_menu_open() {
                    let len = self.filtered_slash_commands().len();
                    if len > 0 {
                        self.slash_selected = (self.slash_selected + 1) % len;
                    }
                } else if alt || ctrl {
                    self.scroll_by(72.0);
                } else {
                    self.active_field_mut().move_end(shift);
                }
            }
            // Page_Up / Page_Down
            0xff55 => self.scroll_by(-240.0),
            0xff56 => self.scroll_by(240.0),
            // Left / Right / Home / End
            0xff51 => {
                if ctrl {
                    self.active_field_mut().move_word_left(shift);
                } else {
                    self.active_field_mut().move_left(shift);
                }
            }
            0xff53 => {
                if ctrl {
                    self.active_field_mut().move_word_right(shift);
                } else {
                    self.active_field_mut().move_right(shift);
                }
            }
            0xff50 => self.active_field_mut().move_home(shift),
            0xff57 => self.active_field_mut().move_end(shift),
            // BackSpace / Delete
            0xff08 => {
                self.active_field_mut().backspace();
                self.slash_selected = 0;
            }
            0xffff => {
                self.active_field_mut().delete_forward();
            }
            _ => {
                if !ctrl && !alt {
                    if let Some(txt) = utf8 {
                        let filtered: String = txt
                            .chars()
                            .filter(|c| !c.is_control() || *c == '\t')
                            .collect();
                        if !filtered.is_empty() {
                            self.active_field_mut().insert_str(&filtered);
                            self.slash_selected = 0;
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
        config.model = "test-model".into(); // no discovery/network in unit tests
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
                if history { app.messages.push(ChatItem::new_user("hello".to_string())); }
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
