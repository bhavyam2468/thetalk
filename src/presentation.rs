//! Shared route transition clock and outgoing-content retention.
//! Geometry remains in RectSpring; content is anchored to the composer, never
//! stretched to the destination or destroyed before contraction completes.
use crate::state::Panel;

pub const TRANSITION_SECONDS: f64 = 0.20;

pub struct PanelTransition {
    panel: Option<Panel>,
    started: f64,
}

impl PanelTransition {
    pub fn new() -> Self { Self { panel: None, started: -TRANSITION_SECONDS } }

    pub fn update(&mut self, panel: Panel, now: f64) -> bool {
        if self.panel == Some(panel) { return false; }
        let changed = self.panel.is_some();
        self.panel = Some(panel);
        self.started = if changed { now } else { now - TRANSITION_SECONDS };
        changed
    }

    pub fn progress(&self, now: f64) -> f32 {
        let t = ((now - self.started) / TRANSITION_SECONDS).clamp(0.0, 1.0) as f32;
        t * t * (3.0 - 2.0 * t)
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
}
