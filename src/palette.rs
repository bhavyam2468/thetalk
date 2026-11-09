use crate::attach::{
    format_bytes, list_dir_entries, quick_attach_dirs, rules, AttachDirEntry, Attachment,
    AttachmentStatus,
};
use crate::config::{ApiFormat, AppConfig, ProviderProfile};
use crate::presentation::RouteKey;
use crate::state::{Panel, TextField};
use crate::theme::ThemeMode;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

pub const MAX_VISIBLE_ROWS: usize = 7;
pub const ROW_STEP: f32 = 36.0;
pub const ROW_HEIGHT: f32 = 32.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteRoute {
    Root,
    Settings,
    Provider,
    Models,
    Theme,
    Attach,
    FileBrowser(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderField {
    ProfileName,
    BaseUrl,
    ApiKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropdownKind {
    Profile,
    Protocol,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteRowAction {
    // Root commands
    OpenSettings,
    OpenProvider,
    OpenModels,
    OpenTheme,
    OpenAttach,
    ToggleThinking,
    CopyLastReply,
    RegenerateLast,
    NewChat,
    HideOverlay,
    QuitApp,

    // Settings rows
    ToggleVision,
    OpenFullSettingsWindow,

    // Provider rows
    ToggleDropdown(DropdownKind),
    EditProviderField(ProviderField),
    ToggleKeyMask,
    CreateCustomProfile,
    DeleteCustomProfile,
    RefreshModels,

    // Model rows
    SelectModel(String),

    // Theme rows
    SelectTheme(ThemeMode),

    // Attach rows
    PasteFromClipboard,
    OpenFileBrowser(PathBuf),
    AttachTypedPath(PathBuf),
    RemoveAttachment(usize),

    // File browser rows
    BrowserParent(PathBuf),
    BrowserEnterDir(PathBuf),
    BrowserPickFile(PathBuf),
}

#[derive(Debug, Clone)]
pub struct PaletteRow {
    pub action: PaletteRowAction,
    pub title: String,
    pub subtitle: Option<String>,
    pub value_badge: Option<String>,
    pub is_submenu: bool,
    pub is_dropdown: bool,
    pub is_active: bool,
    pub editable: Option<ProviderField>,
    pub masked: bool,
}

#[derive(Debug, Clone)]
pub struct DropdownOption {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub is_active: bool,
    #[allow(dead_code)]
    pub is_custom: bool,
}

#[derive(Debug, Clone)]
pub struct PaletteFrame {
    pub route: PaletteRoute,
    pub query: TextField,
    pub selected: usize,
    pub scroll_row: usize,
}

impl PaletteFrame {
    pub fn new(route: PaletteRoute) -> Self {
        Self {
            route,
            query: TextField::default(),
            selected: 0,
            scroll_row: 0,
        }
    }
}

pub struct PaletteState {
    pub open: bool,
    pub stack: Vec<PaletteFrame>,
    pub dropdown: Option<DropdownKind>,
    pub dropdown_selected: usize,
    pub dropdown_scroll: usize,
    pub editing_field: Option<ProviderField>,
    pub edit_buffer: TextField,
    pub dir_entries: Vec<AttachDirEntry>,
    pub dir_error: Option<String>,
}

impl Default for PaletteState {
    fn default() -> Self {
        Self::new()
    }
}

impl PaletteState {
    pub fn new() -> Self {
        Self {
            open: false,
            stack: Vec::new(),
            dropdown: None,
            dropdown_selected: 0,
            dropdown_scroll: 0,
            editing_field: None,
            edit_buffer: TextField::default(),
            dir_entries: Vec::new(),
            dir_error: None,
        }
    }

    pub fn current_route(&self) -> Option<&PaletteRoute> {
        self.stack.last().map(|f| &f.route)
    }

    pub fn current_frame(&self) -> Option<&PaletteFrame> {
        self.stack.last()
    }

    pub fn current_frame_mut(&mut self) -> Option<&mut PaletteFrame> {
        self.stack.last_mut()
    }

    pub fn is_at_root(&self) -> bool {
        self.stack.len() <= 1
            && matches!(self.current_route(), None | Some(PaletteRoute::Root))
    }

    pub fn open_root(&mut self) {
        self.open = true;
        self.dropdown = None;
        self.editing_field = None;
        if self.stack.is_empty() {
            self.stack.push(PaletteFrame::new(PaletteRoute::Root));
        } else {
            self.stack.truncate(1);
            self.stack[0].route = PaletteRoute::Root;
            self.stack[0].selected = 0;
            self.stack[0].scroll_row = 0;
        }
    }

    pub fn push_route(&mut self, route: PaletteRoute) {
        self.open = true;
        self.dropdown = None;
        self.editing_field = None;
        if self.stack.is_empty() {
            self.stack.push(PaletteFrame::new(PaletteRoute::Root));
        }
        if let PaletteRoute::FileBrowser(ref dir) = route {
            self.reload_dir(dir);
        }
        self.stack.push(PaletteFrame::new(route));
    }

    pub fn pop_or_close(&mut self) -> bool {
        self.dropdown = None;
        self.editing_field = None;
        if self.stack.len() > 1 {
            self.stack.pop();
            if let Some(PaletteRoute::FileBrowser(dir)) = self.current_route().cloned() {
                self.reload_dir(&dir);
            }
            true
        } else {
            self.close();
            false
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.stack.clear();
        self.dropdown = None;
        self.editing_field = None;
        self.dir_entries.clear();
        self.dir_error = None;
    }

    pub fn reload_dir(&mut self, dir: &Path) {
        if dir.is_dir() {
            self.dir_entries = list_dir_entries(dir, "");
            self.dir_error = None;
        } else {
            self.dir_entries.clear();
            self.dir_error = Some(format!("Cannot open folder {}", dir.display()));
        }
    }

    /// Unique `RouteKey` used by the shared `PanelTransition` clock across all panels,
    /// submenus, folders, and dropdown drawers.
    pub fn route_key(&self, panel: Panel) -> RouteKey {
        if panel != Panel::Commands {
            return RouteKey::from_panel(panel);
        }
        let depth = self.stack.len().saturating_sub(1) as u8;
        let base_id = match self.current_route() {
            None | Some(PaletteRoute::Root) => 100,
            Some(PaletteRoute::Settings) => 200,
            Some(PaletteRoute::Provider) => 300,
            Some(PaletteRoute::Models) => 400,
            Some(PaletteRoute::Theme) => 500,
            Some(PaletteRoute::Attach) => 600,
            Some(PaletteRoute::FileBrowser(p)) => {
                let mut h = DefaultHasher::new();
                p.hash(&mut h);
                (h.finish() & 0x00ff_ffff) | 0x0100_0000
            }
        };
        match self.dropdown {
            Some(DropdownKind::Profile) => RouteKey::submenu(panel, base_id + 1, depth + 1),
            Some(DropdownKind::Protocol) => RouteKey::submenu(panel, base_id + 2, depth + 1),
            None => RouteKey::submenu(panel, base_id, depth),
        }
    }

    pub fn has_search_bar(&self) -> bool {
        match self.current_route() {
            None | Some(PaletteRoute::Root) => false, // Uses the main composer `/` bar directly when opened via `/`!
            Some(PaletteRoute::Provider) => false,
            Some(PaletteRoute::Theme) => false,
            Some(PaletteRoute::Settings)
            | Some(PaletteRoute::Models)
            | Some(PaletteRoute::Attach)
            | Some(PaletteRoute::FileBrowser(_)) => true,
        }
    }

    pub fn breadcrumbs(&self) -> Vec<String> {
        let mut out = Vec::new();
        for frame in &self.stack {
            let label = match &frame.route {
                PaletteRoute::Root => "Commands".to_string(),
                PaletteRoute::Settings => "Settings".to_string(),
                PaletteRoute::Provider => "Provider".to_string(),
                PaletteRoute::Models => "Models".to_string(),
                PaletteRoute::Theme => "Theme".to_string(),
                PaletteRoute::Attach => "Attach".to_string(),
                PaletteRoute::FileBrowser(p) => p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| p.display().to_string()),
            };
            out.push(label);
        }
        if let Some(dd) = self.dropdown {
            out.push(match dd {
                DropdownKind::Profile => "Profiles".to_string(),
                DropdownKind::Protocol => "Protocol".to_string(),
            });
        }
        out
    }

    pub fn clamp_selection(&mut self, row_count: usize) {
        if let Some(frame) = self.stack.last_mut() {
            if row_count == 0 {
                frame.selected = 0;
                frame.scroll_row = 0;
            } else {
                frame.selected = frame.selected.min(row_count - 1);
                if frame.selected < frame.scroll_row {
                    frame.scroll_row = frame.selected;
                } else if frame.selected >= frame.scroll_row + MAX_VISIBLE_ROWS {
                    frame.scroll_row = frame.selected + 1 - MAX_VISIBLE_ROWS;
                }
                let max_scroll = row_count.saturating_sub(MAX_VISIBLE_ROWS);
                frame.scroll_row = frame.scroll_row.min(max_scroll);
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize, row_count: usize) {
        if let Some(dd) = self.dropdown {
            let dd_len = match dd {
                DropdownKind::Profile => return, // Handled via dropdown_move with live config count
                DropdownKind::Protocol => 2,
            };
            if dd_len > 0 {
                self.dropdown_selected =
                    ((self.dropdown_selected as isize + delta).rem_euclid(dd_len as isize)) as usize;
            }
            return;
        }
        if row_count == 0 {
            return;
        }
        if let Some(frame) = self.stack.last_mut() {
            frame.selected =
                ((frame.selected as isize + delta).rem_euclid(row_count as isize)) as usize;
        }
        self.clamp_selection(row_count);
    }

    pub fn move_dropdown_selection(&mut self, delta: isize, opt_count: usize) {
        if opt_count == 0 {
            return;
        }
        self.dropdown_selected =
            ((self.dropdown_selected as isize + delta).rem_euclid(opt_count as isize)) as usize;
        if self.dropdown_selected < self.dropdown_scroll {
            self.dropdown_scroll = self.dropdown_selected;
        } else if self.dropdown_selected >= self.dropdown_scroll + MAX_VISIBLE_ROWS {
            self.dropdown_scroll = self.dropdown_selected + 1 - MAX_VISIBLE_ROWS;
        }
        let max_scroll = opt_count.saturating_sub(MAX_VISIBLE_ROWS);
        self.dropdown_scroll = self.dropdown_scroll.min(max_scroll);
    }

    pub fn scroll_rows(&mut self, delta: isize, row_count: usize) {
        if let Some(_dd) = self.dropdown {
            let max_s = row_count.saturating_sub(MAX_VISIBLE_ROWS) as isize;
            self.dropdown_scroll = (self.dropdown_scroll as isize + delta).clamp(0, max_s) as usize;
            return;
        }
        if let Some(frame) = self.stack.last_mut() {
            let max_s = row_count.saturating_sub(MAX_VISIBLE_ROWS) as isize;
            frame.scroll_row = (frame.scroll_row as isize + delta).clamp(0, max_s) as usize;
        }
    }

    /// Builds the rows for the active palette route, filtered by the active search/slash query.
    #[allow(clippy::too_many_arguments)]
    pub fn build_rows(
        &self,
        composer_text: &str,
        config: &AppConfig,
        discovered_models: &[String],
        loading_models: bool,
        show_api_key: bool,
        attachments: &[Attachment],
        has_messages: bool,
        has_reasoning: bool,
        has_assistant_reply: bool,
    ) -> Vec<PaletteRow> {
        let route = self.current_route().unwrap_or(&PaletteRoute::Root);
        let q = match route {
            PaletteRoute::Root => {
                let t = composer_text.trim_start();
                if t.starts_with('/') {
                    t.to_ascii_lowercase()
                } else {
                    self.current_frame()
                        .map(|f| f.query.text.trim().to_ascii_lowercase())
                        .unwrap_or_default()
                }
            }
            _ => self
                .current_frame()
                .map(|f| f.query.text.trim().to_ascii_lowercase())
                .unwrap_or_default(),
        };

        match route {
            PaletteRoute::Root => {
                let mut rows = vec![
                    PaletteRow {
                        action: PaletteRowAction::OpenAttach,
                        title: "/attach".into(),
                        subtitle: Some("Attach PDF, image, or file".into()),
                        value_badge: if attachments.is_empty() {
                            None
                        } else {
                            Some(format!("{}/{}", attachments.len(), rules::MAX_ATTACHMENTS))
                        },
                        is_submenu: true,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    },
                    PaletteRow {
                        action: PaletteRowAction::OpenSettings,
                        title: "/settings".into(),
                        subtitle: Some("Provider, model, vision & theme".into()),
                        value_badge: Some(config.active_profile_label().to_string()),
                        is_submenu: true,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    },
                    PaletteRow {
                        action: PaletteRowAction::OpenProvider,
                        title: "/provider".into(),
                        subtitle: Some("Profile, protocol, Base URL & API key".into()),
                        value_badge: Some(config.active_format().label().to_string()),
                        is_submenu: true,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    },
                    PaletteRow {
                        action: PaletteRowAction::OpenModels,
                        title: "/model".into(),
                        subtitle: Some("Choose or search available models".into()),
                        value_badge: if config.model.trim().is_empty() {
                            Some("Auto".into())
                        } else {
                            Some(config.model.clone())
                        },
                        is_submenu: true,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    },
                    PaletteRow {
                        action: PaletteRowAction::OpenTheme,
                        title: "/theme".into(),
                        subtitle: Some("Switch Quiet Paper dark / light".into()),
                        value_badge: Some(match config.theme {
                            ThemeMode::Dark => "Dark".into(),
                            ThemeMode::Light => "Light".into(),
                        }),
                        is_submenu: true,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    },
                ];
                if has_reasoning {
                    rows.push(PaletteRow {
                        action: PaletteRowAction::ToggleThinking,
                        title: "/thinking".into(),
                        subtitle: Some("Toggle latest reasoning disclosure".into()),
                        value_badge: None,
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    });
                }
                if has_assistant_reply {
                    rows.push(PaletteRow {
                        action: PaletteRowAction::CopyLastReply,
                        title: "/copy".into(),
                        subtitle: Some("Copy latest reply to clipboard".into()),
                        value_badge: None,
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    });
                }
                if has_messages {
                    rows.push(PaletteRow {
                        action: PaletteRowAction::RegenerateLast,
                        title: "/retry".into(),
                        subtitle: Some("Regenerate latest reply".into()),
                        value_badge: None,
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    });
                }
                rows.extend([
                    PaletteRow {
                        action: PaletteRowAction::NewChat,
                        title: "/new".into(),
                        subtitle: Some("Reset to thin bar".into()),
                        value_badge: None,
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    },
                    PaletteRow {
                        action: PaletteRowAction::HideOverlay,
                        title: "/hide".into(),
                        subtitle: Some("Dismiss overlay".into()),
                        value_badge: None,
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    },
                    PaletteRow {
                        action: PaletteRowAction::QuitApp,
                        title: "/quit".into(),
                        subtitle: Some("Exit thetalk".into()),
                        value_badge: None,
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    },
                ]);

                if q.is_empty() || q == "/" {
                    rows
                } else {
                    rows.into_iter()
                        .filter(|r| {
                            r.title.to_ascii_lowercase().contains(q.as_str())
                                || r.subtitle
                                    .as_deref()
                                    .unwrap_or("")
                                    .to_ascii_lowercase()
                                    .contains(q.as_str())
                        })
                        .collect()
                }
            }
            PaletteRoute::Settings => {
                let rows = vec![
                    PaletteRow {
                        action: PaletteRowAction::OpenProvider,
                        title: "Provider".into(),
                        subtitle: Some("Profiles, Base URL, API key & protocol".into()),
                        value_badge: Some(format!(
                            "{} · {}",
                            config.active_profile_label(),
                            config.active_format().label()
                        )),
                        is_submenu: true,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    },
                    PaletteRow {
                        action: PaletteRowAction::OpenModels,
                        title: "Model".into(),
                        subtitle: Some("Select or search endpoint models".into()),
                        value_badge: Some(if config.model.trim().is_empty() {
                            "Auto-detect".into()
                        } else {
                            config.model.clone()
                        }),
                        is_submenu: true,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    },
                    PaletteRow {
                        action: PaletteRowAction::ToggleVision,
                        title: "Vision Attachments".into(),
                        subtitle: Some("Send images & scanned PDFs visually to model".into()),
                        value_badge: Some(if config.vision { "On" } else { "Off" }.into()),
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: config.vision,
                        editable: None,
                        masked: false,
                    },
                    PaletteRow {
                        action: PaletteRowAction::OpenTheme,
                        title: "Theme".into(),
                        subtitle: Some("Quiet Paper appearance".into()),
                        value_badge: Some(match config.theme {
                            ThemeMode::Dark => "Dark".into(),
                            ThemeMode::Light => "Light".into(),
                        }),
                        is_submenu: true,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    },
                    PaletteRow {
                        action: PaletteRowAction::OpenAttach,
                        title: "Attachments".into(),
                        subtitle: Some("Attach PDF, image, or code files".into()),
                        value_badge: if attachments.is_empty() {
                            None
                        } else {
                            Some(format!("{} attached", attachments.len()))
                        },
                        is_submenu: true,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    },
                    PaletteRow {
                        action: PaletteRowAction::OpenFullSettingsWindow,
                        title: "Full Settings View".into(),
                        subtitle: Some("Classic form layout".into()),
                        value_badge: None,
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    },
                ];
                filter_rows(rows, &q)
            }
            PaletteRoute::Provider => {
                let active_prof = config.active_profile();
                let is_custom = config.active_profile_is_custom();
                let prof_name = active_prof.name.clone();
                let url_val = active_prof.base_url.clone();
                let key_val = active_prof.api_key.clone();

                let mut rows = vec![
                    PaletteRow {
                        action: PaletteRowAction::ToggleDropdown(DropdownKind::Profile),
                        title: "Profile".into(),
                        subtitle: Some(if is_custom {
                            "Custom saved profile".into()
                        } else {
                            "Built-in provider preset".into()
                        }),
                        value_badge: Some(format!("{}  ▾", prof_name)),
                        is_submenu: false,
                        is_dropdown: true,
                        is_active: self.dropdown == Some(DropdownKind::Profile),
                        editable: None,
                        masked: false,
                    },
                    PaletteRow {
                        action: PaletteRowAction::ToggleDropdown(DropdownKind::Protocol),
                        title: "API Protocol".into(),
                        subtitle: Some("Wire format & attachment encoding".into()),
                        value_badge: Some(format!("{}  ▾", config.active_format().label())),
                        is_submenu: false,
                        is_dropdown: true,
                        is_active: self.dropdown == Some(DropdownKind::Protocol),
                        editable: None,
                        masked: false,
                    },
                ];

                if is_custom {
                    rows.push(PaletteRow {
                        action: PaletteRowAction::EditProviderField(ProviderField::ProfileName),
                        title: "Profile Name".into(),
                        subtitle: Some(prof_name),
                        value_badge: None,
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: self.editing_field == Some(ProviderField::ProfileName),
                        editable: Some(ProviderField::ProfileName),
                        masked: false,
                    });
                }

                rows.push(PaletteRow {
                    action: PaletteRowAction::EditProviderField(ProviderField::BaseUrl),
                    title: "Base URL".into(),
                    subtitle: Some(if url_val.is_empty() {
                        "Empty".into()
                    } else {
                        url_val
                    }),
                    value_badge: None,
                    is_submenu: false,
                    is_dropdown: false,
                    is_active: self.editing_field == Some(ProviderField::BaseUrl),
                    editable: Some(ProviderField::BaseUrl),
                    masked: false,
                });

                rows.push(PaletteRow {
                    action: PaletteRowAction::EditProviderField(ProviderField::ApiKey),
                    title: "API Key".into(),
                    subtitle: Some(if key_val.is_empty() {
                        "Keyless (optional for local servers)".into()
                    } else {
                        key_val.clone()
                    }),
                    value_badge: None,
                    is_submenu: false,
                    is_dropdown: false,
                    is_active: self.editing_field == Some(ProviderField::ApiKey),
                    editable: Some(ProviderField::ApiKey),
                    masked: !show_api_key && !key_val.is_empty(),
                });

                rows.push(PaletteRow {
                    action: PaletteRowAction::ToggleKeyMask,
                    title: "API Key Visibility".into(),
                    subtitle: Some("Toggle masking while editing".into()),
                    value_badge: Some(if show_api_key { "Shown" } else { "Masked" }.into()),
                    is_submenu: false,
                    is_dropdown: false,
                    is_active: show_api_key,
                    editable: None,
                    masked: false,
                });

                rows.push(PaletteRow {
                    action: PaletteRowAction::RefreshModels,
                    title: "Detect Models".into(),
                    subtitle: Some("Probe endpoint /models".into()),
                    value_badge: Some(if loading_models { "..." } else { "Probe" }.into()),
                    is_submenu: false,
                    is_dropdown: false,
                    is_active: false,
                    editable: None,
                    masked: false,
                });

                rows.push(PaletteRow {
                    action: PaletteRowAction::CreateCustomProfile,
                    title: "+ New Custom Profile".into(),
                    subtitle: Some("Add profile with own URL, key & protocol".into()),
                    value_badge: None,
                    is_submenu: false,
                    is_dropdown: false,
                    is_active: false,
                    editable: None,
                    masked: false,
                });

                if is_custom {
                    rows.push(PaletteRow {
                        action: PaletteRowAction::DeleteCustomProfile,
                        title: "Delete Custom Profile".into(),
                        subtitle: Some("Remove this custom profile".into()),
                        value_badge: None,
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    });
                }

                rows
            }
            PaletteRoute::Models => {
                let mut rows = Vec::new();
                rows.push(PaletteRow {
                    action: PaletteRowAction::RefreshModels,
                    title: if loading_models {
                        "Refreshing models...".into()
                    } else {
                        "Refresh models from endpoint".into()
                    },
                    subtitle: Some(config.normalized_base_url()),
                    value_badge: Some("Ctrl+R".into()),
                    is_submenu: false,
                    is_dropdown: false,
                    is_active: false,
                    editable: None,
                    masked: false,
                });

                let raw_q = self
                    .current_frame()
                    .map(|f| f.query.text.trim())
                    .unwrap_or("");
                if !raw_q.is_empty()
                    && !discovered_models
                        .iter()
                        .any(|m| m.eq_ignore_ascii_case(raw_q))
                {
                    rows.push(PaletteRow {
                        action: PaletteRowAction::SelectModel(raw_q.to_string()),
                        title: format!("Use model: {}", raw_q),
                        subtitle: Some("Set custom model ID".into()),
                        value_badge: Some("Custom".into()),
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: config.model == raw_q,
                        editable: None,
                        masked: false,
                    });
                }

                for m in discovered_models {
                    if !q.is_empty() && !m.to_ascii_lowercase().contains(q.as_str()) {
                        continue;
                    }
                    let active = config.model.trim() == m.as_str();
                    rows.push(PaletteRow {
                        action: PaletteRowAction::SelectModel(m.clone()),
                        title: m.clone(),
                        subtitle: None,
                        value_badge: if active { Some("Active".into()) } else { None },
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: active,
                        editable: None,
                        masked: false,
                    });
                }
                rows
            }
            PaletteRoute::Theme => {
                vec![
                    PaletteRow {
                        action: PaletteRowAction::SelectTheme(ThemeMode::Dark),
                        title: "Quiet Paper Dark".into(),
                        subtitle: Some("Warm charcoal #1f1e1c · Copper #d08a5c".into()),
                        value_badge: if config.theme == ThemeMode::Dark {
                            Some("Active".into())
                        } else {
                            None
                        },
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: config.theme == ThemeMode::Dark,
                        editable: None,
                        masked: false,
                    },
                    PaletteRow {
                        action: PaletteRowAction::SelectTheme(ThemeMode::Light),
                        title: "Quiet Paper Light".into(),
                        subtitle: Some("Alabaster #f2eee6 · Sienna #a8623b".into()),
                        value_badge: if config.theme == ThemeMode::Light {
                            Some("Active".into())
                        } else {
                            None
                        },
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: config.theme == ThemeMode::Light,
                        editable: None,
                        masked: false,
                    },
                ]
            }
            PaletteRoute::Attach => {
                let mut rows = Vec::new();
                let raw_q = self
                    .current_frame()
                    .map(|f| f.query.text.trim().to_string())
                    .unwrap_or_default();

                if !raw_q.is_empty() {
                    let candidate = expand_tilde(&raw_q);
                    if candidate.exists() {
                        if candidate.is_dir() {
                            rows.push(PaletteRow {
                                action: PaletteRowAction::OpenFileBrowser(candidate.clone()),
                                title: format!("Open folder: {}", candidate.display()),
                                subtitle: Some("Browse directory".into()),
                                value_badge: Some("Dir".into()),
                                is_submenu: true,
                                is_dropdown: false,
                                is_active: false,
                                editable: None,
                                masked: false,
                            });
                        } else {
                            rows.push(PaletteRow {
                                action: PaletteRowAction::AttachTypedPath(candidate.clone()),
                                title: format!("Attach: {}", candidate.display()),
                                subtitle: Some("Press Enter to attach file".into()),
                                value_badge: Some("File".into()),
                                is_submenu: false,
                                is_dropdown: false,
                                is_active: false,
                                editable: None,
                                masked: false,
                            });
                        }
                    }
                }

                if q.is_empty() {
                    rows.push(PaletteRow {
                        action: PaletteRowAction::PasteFromClipboard,
                        title: "Paste from Clipboard".into(),
                        subtitle: Some("Image, copied file, or path".into()),
                        value_badge: Some("Ctrl+V".into()),
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    });
                }

                for (label, path) in quick_attach_dirs() {
                    if !q.is_empty()
                        && !label.to_ascii_lowercase().contains(q.as_str())
                        && !path.to_string_lossy().to_ascii_lowercase().contains(q.as_str())
                    {
                        continue;
                    }
                    rows.push(PaletteRow {
                        action: PaletteRowAction::OpenFileBrowser(path.clone()),
                        title: format!("Browse {}", label),
                        subtitle: Some(path.display().to_string()),
                        value_badge: None,
                        is_submenu: true,
                        is_dropdown: false,
                        is_active: false,
                        editable: None,
                        masked: false,
                    });
                }

                for (idx, att) in attachments.iter().enumerate() {
                    let badge = match att.status {
                        AttachmentStatus::Loading => "Loading...".into(),
                        AttachmentStatus::Ready => {
                            if let Some(pages) = att.page_count {
                                format!("{} · {}p", format_bytes(att.size_bytes), pages)
                            } else {
                                format_bytes(att.size_bytes)
                            }
                        }
                        AttachmentStatus::Error(_) => "Failed".into(),
                    };
                    if !q.is_empty() && !att.name.to_ascii_lowercase().contains(q.as_str()) {
                        continue;
                    }
                    rows.push(PaletteRow {
                        action: PaletteRowAction::RemoveAttachment(idx),
                        title: format!("Remove {}", att.name),
                        subtitle: Some("Attached to draft".into()),
                        value_badge: Some(badge),
                        is_submenu: false,
                        is_dropdown: false,
                        is_active: true,
                        editable: None,
                        masked: false,
                    });
                }

                rows
            }
            PaletteRoute::FileBrowser(dir) => {
                let mut rows = Vec::new();
                if let Some(parent) = dir.parent() {
                    if q.is_empty() {
                        rows.push(PaletteRow {
                            action: PaletteRowAction::BrowserParent(parent.to_path_buf()),
                            title: "..".into(),
                            subtitle: Some("Parent directory".into()),
                            value_badge: None,
                            is_submenu: false,
                            is_dropdown: false,
                            is_active: false,
                            editable: None,
                            masked: false,
                        });
                    }
                }
                for entry in &self.dir_entries {
                    if !q.is_empty() && !entry.name.to_ascii_lowercase().contains(q.as_str()) {
                        continue;
                    }
                    if entry.is_dir {
                        rows.push(PaletteRow {
                            action: PaletteRowAction::BrowserEnterDir(entry.path.clone()),
                            title: entry.name.clone(),
                            subtitle: Some("Folder".into()),
                            value_badge: None,
                            is_submenu: true,
                            is_dropdown: false,
                            is_active: false,
                            editable: None,
                            masked: false,
                        });
                    } else {
                        rows.push(PaletteRow {
                            action: PaletteRowAction::BrowserPickFile(entry.path.clone()),
                            title: entry.name.clone(),
                            subtitle: Some(format_bytes(entry.size_bytes)),
                            value_badge: entry.kind.map(|k| k.badge().to_string()),
                            is_submenu: false,
                            is_dropdown: false,
                            is_active: false,
                            editable: None,
                            masked: false,
                        });
                    }
                }
                rows
            }
        }
    }

    pub fn build_dropdown_options(
        &self,
        kind: DropdownKind,
        config: &AppConfig,
    ) -> Vec<DropdownOption> {
        match kind {
            DropdownKind::Profile => {
                let mut opts: Vec<DropdownOption> = config
                    .all_profiles()
                    .into_iter()
                    .map(|p| {
                        let is_active = p.id == config.active_profile_id;
                        DropdownOption {
                            id: p.id.clone(),
                            title: p.name.clone(),
                            subtitle: format!("{} · {}", p.format.label(), p.base_url),
                            is_active,
                            is_custom: !p.builtin,
                        }
                    })
                    .collect();
                opts.push(DropdownOption {
                    id: "__new_custom__".into(),
                    title: "+ Create Custom Profile".into(),
                    subtitle: "Add new endpoint with custom URL & API key".into(),
                    is_active: false,
                    is_custom: true,
                });
                opts
            }
            DropdownKind::Protocol => {
                let active = config.active_format();
                vec![
                    DropdownOption {
                        id: "openai".into(),
                        title: "OpenAI-compatible".into(),
                        subtitle: "/chat/completions · image_url multimodal".into(),
                        is_active: active == ApiFormat::OpenAi,
                        is_custom: false,
                    },
                    DropdownOption {
                        id: "anthropic".into(),
                        title: "Anthropic Messages".into(),
                        subtitle: "/messages · native PDF document & image blocks".into(),
                        is_active: active == ApiFormat::Anthropic,
                        is_custom: false,
                    },
                ]
            }
        }
    }
}

fn filter_rows(rows: Vec<PaletteRow>, q: &str) -> Vec<PaletteRow> {
    if q.is_empty() {
        return rows;
    }
    rows.into_iter()
        .filter(|r| {
            r.title.to_ascii_lowercase().contains(q)
                || r.subtitle
                    .as_deref()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains(q)
                || r.value_badge
                    .as_deref()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains(q)
        })
        .collect()
}

pub fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

pub fn profile_from_dropdown_selection(
    config: &mut AppConfig,
    opt_id: &str,
) -> Option<ProviderProfile> {
    if opt_id == "__new_custom__" {
        let p = config.add_custom_profile("Custom", ApiFormat::OpenAi, "", "");
        return Some(p);
    }
    if config.select_profile(opt_id) {
        return Some(config.active_profile().clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_key_tracks_submenu_depth_and_dropdowns() {
        let mut p = PaletteState::new();
        p.open_root();
        assert_eq!(
            p.route_key(Panel::Commands),
            RouteKey::submenu(Panel::Commands, 100, 0)
        );
        p.push_route(PaletteRoute::Settings);
        assert_eq!(
            p.route_key(Panel::Commands),
            RouteKey::submenu(Panel::Commands, 200, 1)
        );
        p.push_route(PaletteRoute::Provider);
        assert_eq!(
            p.route_key(Panel::Commands),
            RouteKey::submenu(Panel::Commands, 300, 2)
        );
        p.dropdown = Some(DropdownKind::Profile);
        assert_eq!(
            p.route_key(Panel::Commands),
            RouteKey::submenu(Panel::Commands, 301, 3)
        );
        assert!(p.pop_or_close());
        assert_eq!(p.current_route(), Some(&PaletteRoute::Settings));
        assert!(p.pop_or_close());
        assert_eq!(p.current_route(), Some(&PaletteRoute::Root));
        assert!(!p.pop_or_close());
        assert!(!p.open);
    }

    #[test]
    fn model_list_scales_and_virtualizes_cleanly() {
        let mut p = PaletteState::new();
        p.push_route(PaletteRoute::Models);
        let cfg = AppConfig::default();
        let models: Vec<String> = (0..5000).map(|i| format!("model-{}", i)).collect();
        let rows = p.build_rows("", &cfg, &models, false, false, &[], false, false, false);
        assert_eq!(rows.len(), 5001); // Refresh + 5000 models

        p.move_selection(250, rows.len());
        let frame = p.current_frame().unwrap();
        assert_eq!(frame.selected, 250);
        assert!(frame.scroll_row <= 250 && frame.selected < frame.scroll_row + MAX_VISIBLE_ROWS);
    }
}
