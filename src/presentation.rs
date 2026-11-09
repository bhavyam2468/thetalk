//! Shared route transition clock and outgoing-content retention.
//! Geometry remains in RectSpring; content is anchored to the composer, never
//! stretched to the destination or destroyed before contraction completes.
use crate::state::Panel;

pub const TRANSITION_SECONDS: f64 = 0.20;

/// Unique route identity covering both top-level panels and nested palette submenus / dropdowns.
/// Any change to `RouteKey` triggers the exact same shared `PanelTransition` clock and
/// outgoing-panel cross-fade + depth slide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteKey {
    pub panel: Panel,
    pub sub_id: u64,
    pub depth: u8,
}

impl RouteKey {
    pub const fn from_panel(panel: Panel) -> Self {
        Self {
            panel,
            sub_id: 0,
            depth: 0,
        }
    }

    pub const fn submenu(panel: Panel, sub_id: u64, depth: u8) -> Self {
        Self {
            panel,
            sub_id,
            depth,
        }
    }
}

pub struct PanelTransition {
    route: Option<RouteKey>,
    started: f64,
    /// `+1` when opening or drilling deeper, `-1` when closing or backing up, `0` lateral.
    direction: i8,
}

impl PanelTransition {
    pub fn new() -> Self {
        Self {
            route: None,
            started: -TRANSITION_SECONDS,
            direction: 0,
        }
    }

    pub fn update_route(&mut self, route: RouteKey, now: f64) -> bool {
        if self.route == Some(route) {
            return false;
        }
        let changed = self.route.is_some();
        self.direction = match self.route {
            None => 0,
            Some(prev) => {
                if prev.panel == Panel::Commands && route.panel == Panel::Commands {
                    if route.depth > prev.depth {
                        1
                    } else if route.depth < prev.depth {
                        -1
                    } else if route.sub_id != prev.sub_id {
                        0
                    } else {
                        0
                    }
                } else if route.panel == Panel::Commands && prev.panel != Panel::Commands {
                    1
                } else if prev.panel == Panel::Commands && route.panel != Panel::Commands {
                    -1
                } else {
                    0
                }
            }
        };
        self.route = Some(route);
        self.started = if changed { now } else { now - TRANSITION_SECONDS };
        changed
    }

    #[allow(dead_code)]
    pub fn update(&mut self, panel: Panel, now: f64) -> bool {
        self.update_route(RouteKey::from_panel(panel), now)
    }

    pub fn progress(&self, now: f64) -> f32 {
        let t = ((now - self.started) / TRANSITION_SECONDS).clamp(0.0, 1.0) as f32;
        t * t * (3.0 - 2.0 * t)
    }

    pub fn direction(&self) -> i8 {
        self.direction
    }
}

/// A retained *composited* panel, allowing interruption of any transition
/// without jumping back to an old source scene. It contains no live hitboxes.
pub struct PanelSnapshot {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_routes_use_the_same_interruptible_clock() {
        let routes = [Panel::Compact, Panel::Commands, Panel::Conversation, Panel::Settings];
        for from in routes {
            for to in routes {
                let mut transition = PanelTransition::new();
                assert!(!transition.update(from, 0.0));
                assert_eq!(transition.progress(0.0), 1.0);
                assert_eq!(transition.update(to, 1.0), from != to);
                if from != to {
                    assert_eq!(transition.progress(1.0), 0.0);
                    assert!((transition.progress(1.1) - 0.5).abs() < 0.001);
                    assert_eq!(transition.progress(2.0), 1.0);
                    assert!(transition.update(from, 1.1));
                    assert_eq!(transition.progress(1.1), 0.0);
                }
            }
        }
    }

    #[test]
    fn submenu_drill_in_and_back_track_direction_on_shared_clock() {
        let mut t = PanelTransition::new();
        assert!(!t.update_route(RouteKey::submenu(Panel::Commands, 1, 0), 0.0));
        assert!(t.update_route(RouteKey::submenu(Panel::Commands, 2, 1), 1.0));
        assert_eq!(t.direction(), 1);
        assert_eq!(t.progress(1.0), 0.0);
        // Interrupt mid-flight and drill deeper to depth 2
        assert!(t.update_route(RouteKey::submenu(Panel::Commands, 3, 2), 1.05));
        assert_eq!(t.direction(), 1);
        assert_eq!(t.progress(1.05), 0.0);
        // Back out to depth 1
        assert!(t.update_route(RouteKey::submenu(Panel::Commands, 2, 1), 1.10));
        assert_eq!(t.direction(), -1);
    }
}
