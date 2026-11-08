use serde::{Deserialize, Serialize};
use tiny_skia::Color;

/// Unified corner radius token (`--r: 12px` in final-chat's Quiet Paper design system).
#[allow(dead_code)]
pub const RADIUS: f32 = 12.0;
/// Proportional inner radius for nested rows (`calc(var(--r) - 4px)`).
#[allow(dead_code)]
pub const RADIUS_INNER: f32 = 8.0;
/// Inline codespan pill radius (`5px`).
#[allow(dead_code)]
pub const RADIUS_CODESPAN: f32 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Dark,
    Light,
}

impl Default for ThemeMode {
    fn default() -> Self {
        Self::Dark
    }
}

impl ThemeMode {
    pub fn toggle(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }

    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark · Quiet Charcoal",
            Self::Light => "Light · Warm Paper",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn hex(rgb: u32) -> Self {
        Self {
            r: ((rgb >> 16) & 0xFF) as u8,
            g: ((rgb >> 8) & 0xFF) as u8,
            b: (rgb & 0xFF) as u8,
            a: 255,
        }
    }

    pub fn with_alpha(self, alpha: f32) -> Self {
        let a = ((self.a as f32) * alpha.clamp(0.0, 1.0)).round() as u8;
        Self { a, ..self }
    }

    #[allow(dead_code)]
    pub fn to_skia(self) -> Color {
        Color::from_rgba8(self.r, self.g, self.b, self.a)
    }

    pub fn to_cosmic(self) -> cosmic_text::Color {
        cosmic_text::Color::rgba(self.r, self.g, self.b, self.a)
    }

    /// Premultiplied-alpha color interpolation (ported from DankMaterialShell `DColorAnimation.qml`),
    /// preventing dark/gray fringes when transitioning between translucent and opaque colors.
    pub fn lerp_premultiplied(from: Self, to: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let fa = from.a as f32 / 255.0;
        let ta = to.a as f32 / 255.0;
        let out_a = fa + (ta - fa) * t;
        if out_a <= 1e-5 {
            return Self::new(to.r, to.g, to.b, 0);
        }
        let mix = |fc: u8, tc: u8| -> u8 {
            let f_pre = (fc as f32 / 255.0) * fa;
            let t_pre = (tc as f32 / 255.0) * ta;
            let c = (f_pre + (t_pre - f_pre) * t) / out_a;
            (c.clamp(0.0, 1.0) * 255.0).round() as u8
        };
        Self::new(
            mix(from.r, to.r),
            mix(from.g, to.g),
            mix(from.b, to.b),
            (out_a.clamp(0.0, 1.0) * 255.0).round() as u8,
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub mode: ThemeMode,
    #[allow(dead_code)]
    pub bg: Rgba,
    pub float: Rgba,
    pub bubble: Rgba,
    pub surface: Rgba,
    pub surface_hover: Rgba,
    pub fg: Rgba,
    pub muted: Rgba,
    pub faint: Rgba,
    pub line: Rgba,
    pub accent: Rgba,
    pub success: Rgba,
    pub danger: Rgba,
    pub mark: Rgba,
    pub shadow_alpha: f32,
    pub specular_inset: Rgba,
    pub grain: f32,
    // Restrained Quiet Paper code syntax palette
    pub syn_keyword: Rgba,
    pub syn_string: Rgba,
    pub syn_number: Rgba,
    pub syn_type: Rgba,
    pub syn_comment: Rgba,
}

impl Theme {
    pub fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self {
                mode,
                bg: Rgba::hex(0x1f1e1c),
                float: Rgba::hex(0x252422),
                bubble: Rgba::hex(0x2b2a27),
                surface: Rgba::new(230, 225, 216, 12),       // ~0.047
                surface_hover: Rgba::new(230, 225, 216, 20), // ~0.078
                fg: Rgba::hex(0xe6e1d8),
                muted: Rgba::hex(0x8e887e),
                faint: Rgba::hex(0x5f5a53),
                line: Rgba::new(230, 225, 216, 22),          // ~0.086
                accent: Rgba::hex(0xd08a5c),
                success: Rgba::hex(0x8fb08c),
                danger: Rgba::hex(0xd9776b),
                mark: Rgba::new(208, 160, 80, 77),           // 0.30
                shadow_alpha: 0.56,
                specular_inset: Rgba::new(255, 255, 255, 7), // subtle top highlight
                grain: 0.030,
                syn_keyword: Rgba::hex(0xd08a5c),
                syn_string: Rgba::hex(0x9bb898),
                syn_number: Rgba::hex(0xd4a96a),
                syn_type: Rgba::hex(0xdfd0b8),
                syn_comment: Rgba::hex(0x6b655c),
            },
            ThemeMode::Light => Self {
                mode,
                bg: Rgba::hex(0xf2eee6),
                float: Rgba::hex(0xf6f3ec),
                bubble: Rgba::hex(0xe8e2d7),
                surface: Rgba::new(42, 40, 36, 12),          // ~0.047
                surface_hover: Rgba::new(42, 40, 36, 21),    // ~0.082
                fg: Rgba::hex(0x2a2824),
                muted: Rgba::hex(0x8b857b),
                faint: Rgba::hex(0xb3ada2),
                line: Rgba::new(42, 40, 36, 26),             // ~0.10
                accent: Rgba::hex(0xa8623b),
                success: Rgba::hex(0x5b7f5a),
                danger: Rgba::hex(0xb24a3f),
                mark: Rgba::new(214, 170, 88, 97),           // 0.38
                shadow_alpha: 0.22,
                specular_inset: Rgba::new(255, 255, 255, 90),
                grain: 0.044,
                syn_keyword: Rgba::hex(0xa8623b),
                syn_string: Rgba::hex(0x4f734e),
                syn_number: Rgba::hex(0x966b28),
                syn_type: Rgba::hex(0x5c4d38),
                syn_comment: Rgba::hex(0x9c958a),
            },
        }
    }
}

/// Deterministic 128x128 tileable monochrome film-grain LUT (`--grain` in final-chat).
/// Computed once at startup so applying tactile paper texture costs nanoseconds per pixel.
pub struct GrainLut {
    values: [i8; 128 * 128],
}

impl GrainLut {
    pub fn new() -> Self {
        let mut values = [0i8; 128 * 128];
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        for slot in values.iter_mut() {
            // SplitMix64 hash for zero-pattern high-frequency film grain
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            // Triangular distribution in [-64, 64] for natural analog film feel
            let a = ((z & 0x7F) as i16) - 64;
            let b = (((z >> 8) & 0x7F) as i16) - 64;
            *slot = ((a + b) / 2) as i8;
        }
        Self { values }
    }

    #[inline(always)]
    pub fn sample(&self, x: usize, y: usize) -> i16 {
        self.values[((y & 127) << 7) | (x & 127)] as i16
    }
}

impl Default for GrainLut {
    fn default() -> Self {
        Self::new()
    }
}
