/// Smooth streaming text pacer (`useSmoothText.ts` port) + optical blur-to-focus word frontier
/// + `<think>...</think>` reasoning block splitter for local reasoning models (DeepSeek-R1, QwQ, Qwen3).

/// Duration (in seconds) over which each newly revealed word pulls from soft Gaussian blur into sharp focus.
pub const WORD_FOCUS_DURATION: f64 = 0.62;
/// Maximum simultaneous frontier words kept in the optical blur pipeline.
pub const MAX_FRONTIER_WORDS: usize = 28;

const MIN_CPS: f64 = 48.0;
const MAX_LAG_SECS: f64 = 0.65;
const DRAIN_SECS: f64 = 0.22;

#[derive(Debug, Clone, Copy)]
pub struct FrontierWord {
    /// Byte range within the cleaned answer markdown string.
    pub start: usize,
    pub end: usize,
    pub birth_secs: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct WordFocusState {
    /// Logical Gaussian blur sigma (multiply by display scale factor for physical pixels).
    pub sigma_logical: f32,
    /// Opacity `[0.0..=1.0]`.
    pub opacity: f32,
    /// Subtle upward float offset in logical pixels.
    pub offset_y: f32,
}

impl FrontierWord {
    pub fn evaluate(&self, now_secs: f64) -> Option<WordFocusState> {
        let age = (now_secs - self.birth_secs).max(0.0);
        if age >= WORD_FOCUS_DURATION {
            return None;
        }
        let u = (age / WORD_FOCUS_DURATION).clamp(0.0, 1.0) as f32;
        let rem = 1.0 - u;
        // Present optical focus pull: opacity rises briskly so the blurred silhouette is genuinely
        // visible while Gaussian sigma smoothly condenses from 6.6px -> 0.0px.
        let sigma_logical = 6.6 * rem.powf(1.85);
        let opacity = (0.16 + 0.84 * (1.0 - rem.powf(2.8))).clamp(0.0, 1.0);
        let offset_y = 3.2 * rem.powi(3);
        Some(WordFocusState {
            sigma_logical,
            opacity,
            offset_y,
        })
    }
}

#[derive(Debug, Clone)]
pub struct SplitReasoning<'a> {
    pub reasoning: Option<&'a str>,
    pub reasoning_active: bool,
    pub answer: &'a str,
}

/// Splits leading `<think>...</think>` or `<thinking>...</thinking>` blocks emitted by local
/// reasoning models so raw XML tags never pollute the rendered Markdown answer.
pub fn split_reasoning(raw: &str) -> SplitReasoning<'_> {
    let trimmed = raw.trim_start();
    let (open_tag, close_tag) = if trimmed.starts_with("<think>") {
        ("<think>", "</think>")
    } else if trimmed.starts_with("<thinking>") {
        ("<thinking>", "</thinking>")
    } else {
        return SplitReasoning {
            reasoning: None,
            reasoning_active: false,
            answer: raw,
        };
    };

    let body = &trimmed[open_tag.len()..];
    if let Some(close_idx) = body.find(close_tag) {
        let thought = body[..close_idx].trim();
        let after = body[close_idx + close_tag.len()..].trim_start_matches(['\r', '\n']);
        SplitReasoning {
            reasoning: if thought.is_empty() {
                None
            } else {
                Some(thought)
            },
            reasoning_active: false,
            answer: after,
        }
    } else {
        SplitReasoning {
            reasoning: Some(body.trim()),
            reasoning_active: true,
            answer: "",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SmoothStream {
    raw_answer: String,
    pub reasoning: Option<String>,
    pub reasoning_active: bool,
    pub reasoning_started_secs: Option<f64>,
    pub reasoning_duration_secs: Option<f64>,
    revealed_bytes: usize,
    streaming: bool,
    rate_cps: f64,
    last_chunk_len: usize,
    last_chunk_secs: Option<f64>,
    carry_chars: f64,
    frontier: Vec<FrontierWord>,
    pub reasoning_frontier: Vec<FrontierWord>,
}

impl Default for SmoothStream {
    fn default() -> Self {
        Self::new()
    }
}

impl SmoothStream {
    pub fn new() -> Self {
        Self {
            raw_answer: String::new(),
            reasoning: None,
            reasoning_active: false,
            reasoning_started_secs: None,
            reasoning_duration_secs: None,
            revealed_bytes: 0,
            streaming: false,
            rate_cps: 85.0,
            last_chunk_len: 0,
            last_chunk_secs: None,
            carry_chars: 0.0,
            frontier: Vec::with_capacity(MAX_FRONTIER_WORDS),
            reasoning_frontier: Vec::with_capacity(MAX_FRONTIER_WORDS),
        }
    }

    pub fn from_completed(text: &str) -> Self {
        let split = split_reasoning(text);
        Self {
            raw_answer: split.answer.to_string(),
            reasoning: split.reasoning.map(|s| s.to_string()),
            reasoning_active: false,
            reasoning_started_secs: None,
            reasoning_duration_secs: None,
            revealed_bytes: split.answer.len(),
            streaming: false,
            rate_cps: 120.0,
            last_chunk_len: split.answer.len(),
            last_chunk_secs: None,
            carry_chars: 0.0,
            frontier: Vec::new(),
            reasoning_frontier: Vec::new(),
        }
    }

    pub fn start_streaming(&mut self, now_secs: f64) {
        self.raw_answer.clear();
        self.reasoning = None;
        self.reasoning_active = false;
        self.reasoning_started_secs = None;
        self.reasoning_duration_secs = None;
        self.revealed_bytes = 0;
        self.streaming = true;
        self.rate_cps = 85.0;
        self.last_chunk_len = 0;
        self.last_chunk_secs = Some(now_secs);
        self.carry_chars = 0.0;
        self.frontier.clear();
        self.reasoning_frontier.clear();
    }

    pub fn update_raw(&mut self, full_raw: &str, streaming: bool, now_secs: f64) {
        let split = split_reasoning(full_raw);
        if split.reasoning.is_some() && self.reasoning_started_secs.is_none() {
            self.reasoning_started_secs = Some(now_secs);
        }
        if self.reasoning_active && !split.reasoning_active {
            if let Some(t0) = self.reasoning_started_secs {
                self.reasoning_duration_secs = Some((now_secs - t0).max(0.1));
            }
        }
        let old_len = self.reasoning.as_deref().map(str::len).unwrap_or(0);
        if let Some(text) = split.reasoning {
            if text.len() < old_len { self.reasoning_frontier.clear(); }
            let start = old_len.min(text.len());
            if text.is_char_boundary(start) {
                let mut offset = start;
                for word in text[start..].split_inclusive(char::is_whitespace) {
                    let end = offset + word.len();
                    if !word.trim().is_empty() {
                        self.reasoning_frontier.push(FrontierWord { start: offset, end, birth_secs: now_secs });
                    }
                    offset = end;
                }
            }
            if self.reasoning_frontier.len() > MAX_FRONTIER_WORDS {
                let excess = self.reasoning_frontier.len() - MAX_FRONTIER_WORDS;
                self.reasoning_frontier.drain(..excess);
            }
        }
        self.reasoning = split.reasoning.map(|s| s.to_string());
        self.reasoning_active = split.reasoning_active && streaming;

        let new_answer = split.answer;
        let new_len = new_answer.len();
        if let Some(prev_t) = self.last_chunk_secs {
            let dt = now_secs - prev_t;
            let added = new_len.saturating_sub(self.last_chunk_len);
            if dt > 0.008 && added > 0 {
                let measured = ((added as f64) / dt).min(4000.0);
                self.rate_cps = self.rate_cps * 0.70 + measured * 0.30;
                self.last_chunk_secs = Some(now_secs);
                self.last_chunk_len = new_len;
            }
        } else {
            self.last_chunk_secs = Some(now_secs);
            self.last_chunk_len = new_len;
        }

        if new_len < self.revealed_bytes {
            self.revealed_bytes = new_len;
            self.frontier.retain(|w| w.end <= new_len);
        }
        self.raw_answer.clear();
        self.raw_answer.push_str(new_answer);
        self.streaming = streaming;
    }

    /// Advances smooth word reveal and prunes expired blur-to-focus frontier words.
    /// Returns `true` if another animation frame is required.
    pub fn tick(&mut self, dt_secs: f64, now_secs: f64) -> bool {
        let dt = if dt_secs.is_finite() { dt_secs.max(0.0) } else { 0.0 };
        let backlog = self.raw_answer.len().saturating_sub(self.revealed_bytes);

        if backlog > 0 && dt > 0.0 {
            let cps = if !self.streaming {
                ((backlog as f64) / DRAIN_SECS).max(420.0)
            } else {
                MIN_CPS
                    .max(self.rate_cps * 0.96)
                    .max((backlog as f64) / MAX_LAG_SECS)
            };
            self.carry_chars += cps * dt;
            let step = self.carry_chars.floor() as usize;
            if step >= 1 {
                self.carry_chars -= step as f64;
                let next = self.snap_word_boundary(self.revealed_bytes + step);
                if next > self.revealed_bytes {
                    self.record_frontier_words(self.revealed_bytes, next, now_secs);
                    self.revealed_bytes = next;
                }
            }
        }

        self.frontier
            .retain(|w| (now_secs - w.birth_secs) < WORD_FOCUS_DURATION);
        self.reasoning_frontier.retain(|w| now_secs - w.birth_secs < 0.24);

        self.is_animating(now_secs)
    }

    /// Force-reveals a specific byte range with staggered birth timestamps (used by `--render-preview`
    /// to showcase deterministic mid-flight optical blur-to-focus across the trailing prose frontier).
    pub fn seed_preview_frontier(&mut self, full_markdown: &str, now_secs: f64) {
        self.raw_answer = full_markdown.to_string();
        self.revealed_bytes = full_markdown.len();
        self.streaming = true;
        self.frontier.clear();

        // Collect last 14 word spans in the final paragraph and stagger their birth times
        let mut word_spans: Vec<(usize, usize)> = Vec::new();
        let bytes = full_markdown.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i > start {
                word_spans.push((start, i));
            }
        }

        let count = word_spans.len().min(14);
        let tail = &word_spans[word_spans.len().saturating_sub(count)..];
        for (idx, &(start, end)) in tail.iter().enumerate() {
            // Older words near the left of the frontier are nearly focused; newest word at the tip is deeply blurred
            let frac_from_newest = (count - 1 - idx) as f64 / (count.max(1) as f64);
            let age = frac_from_newest * (WORD_FOCUS_DURATION * 0.88);
            self.frontier.push(FrontierWord {
                start,
                end,
                birth_secs: now_secs - age,
            });
        }
    }

    fn snap_word_boundary(&self, mut candidate: usize) -> usize {
        let s = &self.raw_answer;
        if candidate >= s.len() {
            return s.len();
        }
        while candidate < s.len() && !s.is_char_boundary(candidate) {
            candidate += 1;
        }
        if candidate >= s.len() {
            return s.len();
        }
        let rest = &s[candidate..];
        if let Some(first_ch) = rest.chars().next() {
            if !first_ch.is_whitespace() {
                // Look ahead up to 16 bytes to keep words intact
                let mut offset = 0usize;
                for ch in rest.chars() {
                    if ch.is_whitespace() {
                        return candidate + offset;
                    }
                    offset += ch.len_utf8();
                    if offset >= 16 {
                        break;
                    }
                }
                if self.streaming && s.len() - candidate < 16 {
                    return self.revealed_bytes;
                }
            }
        }
        candidate
    }

    fn record_frontier_words(&mut self, from: usize, to: usize, now_secs: f64) {
        let slice = &self.raw_answer[from..to];
        let mut local = 0usize;
        let bytes = slice.as_bytes();
        let mut newly_added = 0usize;
        while local < bytes.len() {
            while local < bytes.len() && bytes[local].is_ascii_whitespace() {
                local += 1;
            }
            let w_start = local;
            while local < bytes.len() && !bytes[local].is_ascii_whitespace() {
                local += 1;
            }
            if local > w_start {
                // Slight 14ms intra-burst stagger when multiple words arrive in one frame
                let stagger = (newly_added.min(6) as f64) * 0.014;
                self.frontier.push(FrontierWord {
                    start: from + w_start,
                    end: from + local,
                    birth_secs: now_secs + stagger,
                });
                newly_added += 1;
            }
        }
        if self.frontier.len() > MAX_FRONTIER_WORDS {
            let excess = self.frontier.len() - MAX_FRONTIER_WORDS;
            self.frontier.drain(0..excess);
        }
    }

    pub fn shown_text(&self) -> &str {
        &self.raw_answer[..self.revealed_bytes.min(self.raw_answer.len())]
    }

    pub fn full_answer(&self) -> &str {
        &self.raw_answer
    }

    pub fn frontier(&self) -> &[FrontierWord] {
        &self.frontier
    }

    pub fn is_live(&self) -> bool {
        self.streaming || self.revealed_bytes < self.raw_answer.len()
    }

    pub fn is_animating(&self, now_secs: f64) -> bool {
        self.streaming
            || !self.reasoning_frontier.is_empty()
            || self.revealed_bytes < self.raw_answer.len()
            || self
                .frontier
                .iter()
                .any(|w| (now_secs - w.birth_secs) < WORD_FOCUS_DURATION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_think_blocks_cleanly() {
        let s1 = split_reasoning("<think>step 1</think>\nFinal **answer**.");
        assert_eq!(s1.reasoning, Some("step 1"));
        assert!(!s1.reasoning_active);
        assert_eq!(s1.answer, "Final **answer**.");

        let s2 = split_reasoning("<think>still thinking...");
        assert_eq!(s2.reasoning, Some("still thinking..."));
        assert!(s2.reasoning_active);
        assert_eq!(s2.answer, "");
    }

    #[test]
    fn smooth_stream_reveals_whole_words_and_tracks_blur_frontier() {
        let mut st = SmoothStream::new();
        st.start_streaming(0.0);
        st.update_raw("Fluid optical focus pull on Linux.", true, 0.02);
        st.tick(0.12, 0.14);
        let shown = st.shown_text();
        assert!(!shown.is_empty());
        assert!(!st.frontier().is_empty());
        let state = st.frontier().first().unwrap().evaluate(0.20).unwrap();
        assert!(state.sigma_logical > 2.0 && state.opacity > 0.25);
        let state_last = st.frontier().last().unwrap().evaluate(0.28).unwrap();
        assert!(state_last.sigma_logical > 2.0 && state_last.opacity > 0.25);
    }
}
