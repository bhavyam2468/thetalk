/// Material 3 Expressive analytical spring physics engine,
/// faithfully ported from DankMaterialShell (`SpringMath.js`, `SpringMotion.qml`, `Style.qml`, `FluidGeometry.js`).

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringSpec {
    pub stiffness: f32,
    pub damping: f32,
    pub mass: f32,
}

impl SpringSpec {
    /// M3 Expressive `defaultSpatial` — fluid container expansion with physical mass and subtle settle.
    pub const DEFAULT_SPATIAL: Self = Self {
        stiffness: 380.0,
        damping: 29.2,
        mass: 1.0,
    };

    /// M3 Expressive `fastSpatial` — snappy micro-interactions (send button pop, row highlight).
    pub const FAST_SPATIAL: Self = Self {
        stiffness: 720.0,
        damping: 35.0,
        mass: 1.0,
    };

    /// Heavy fluid sheet motion (`RectSpringMotion` tuned for summon/dismiss).
    pub const SHEET_MORPH: Self = Self {
        stiffness: 330.0,
        damping: 27.5,
        mass: 1.04,
    };

    /// Critically-damped M3 Expressive `defaultEffects` — opacity/color/scroll (never overshoots).
    #[allow(dead_code)]
    pub const EFFECTS: Self = Self {
        stiffness: 1500.0,
        damping: 80.0,
        mass: 1.0,
    };

    /// Smooth kinetic scroll spring (critically damped, responsive).
    pub const SCROLL: Self = Self {
        stiffness: 620.0,
        damping: 50.0,
        mass: 1.0,
    };
}

/// Closed-form 2x2 state-transition matrix `[m00, m01, m10, m11]` for the second-order ODE:
/// `m * x'' + c * x' + k * (x - target) = 0` over exact elapsed `dt` seconds.
///
/// Exact analytical solution (ported from DankMaterialShell `SpringMath.js`) — unconditionally
/// stable for any timestep `dt > 0` and frame-rate invariant across 60Hz / 120Hz / 240Hz displays.
#[inline]
pub fn spring_coefficients(stiffness: f32, damping: f32, mass: f32, seconds: f32) -> [f32; 4] {
    if !seconds.is_finite() || !stiffness.is_finite() || !damping.is_finite() || !mass.is_finite() {
        return [0.0, 0.0, 0.0, 0.0];
    }
    if seconds <= 0.0 {
        return [1.0, 0.0, 0.0, 1.0];
    }
    let dt = seconds.min(0.1);
    let inv_mass = 1.0 / mass.max(0.001);
    let omega_sq = stiffness.max(0.0) * inv_mass;
    let decay = damping.max(0.0) * inv_mass * 0.5;
    let discriminant = decay * decay - omega_sq;
    let tolerance = 1e-7 * omega_sq.max(1.0);

    if discriminant > tolerance {
        // Overdamped
        let root = discriminant.sqrt();
        let slow = -omega_sq / (decay + root);
        let fast = -decay - root;
        let slow_exp = (slow * dt).exp();
        let fast_exp = (fast * dt).exp();
        let divisor = slow - fast;
        let b = (slow_exp - fast_exp) / divisor;
        [
            (-fast * slow_exp + slow * fast_exp) / divisor,
            b,
            -omega_sq * b,
            (slow * slow_exp - fast * fast_exp) / divisor,
        ]
    } else {
        let envelope = (-decay * dt).exp();
        let (cosine, sine) = if discriminant < -tolerance {
            // Underdamped (expressive momentum)
            let freq = (-discriminant).sqrt();
            let (s, c) = (freq * dt).sin_cos();
            (c, s / freq)
        } else {
            // Critically damped
            (1.0, dt)
        };
        let b = envelope * sine;
        [
            envelope * cosine + decay * b,
            b,
            -omega_sq * b,
            envelope * cosine - decay * b,
        ]
    }
}

#[derive(Debug, Clone)]
pub struct Spring {
    pub spec: SpringSpec,
    pub value: f32,
    pub target: f32,
    pub velocity: f32,
    pub pos_eps: f32,
    pub vel_eps: f32,
    running: bool,
}

impl Spring {
    pub fn new(initial: f32, spec: SpringSpec) -> Self {
        Self {
            spec,
            value: initial,
            target: initial,
            velocity: 0.0,
            pos_eps: 0.02,
            vel_eps: 0.05,
            running: false,
        }
    }

    pub fn with_epsilon(mut self, pos_eps: f32, vel_eps: f32) -> Self {
        self.pos_eps = pos_eps;
        self.vel_eps = vel_eps;
        self
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn snap_to(&mut self, v: f32) {
        self.value = v;
        self.target = v;
        self.velocity = 0.0;
        self.running = false;
    }

    pub fn retarget(&mut self, v: f32) {
        if (self.target - v).abs() <= self.pos_eps * 0.25 && !self.running {
            return;
        }
        self.target = v;
        if (self.target - self.value).abs() > self.pos_eps || self.velocity.abs() > self.vel_eps {
            self.running = true;
        } else {
            self.snap_to(v);
        }
    }

    #[allow(dead_code)]
    pub fn impulse(&mut self, dv: f32) {
        self.velocity += dv;
        self.running = true;
    }

    /// Advances the spring by `dt` seconds. Returns `true` if still in motion.
    pub fn advance(&mut self, dt: f32) -> bool {
        if !self.running {
            return false;
        }
        if dt <= 0.0 {
            return self.running;
        }
        let m = spring_coefficients(self.spec.stiffness, self.spec.damping, self.spec.mass, dt);
        let disp = self.value - self.target;
        let next_val = self.target + m[0] * disp + m[1] * self.velocity;
        let next_vel = m[2] * disp + m[3] * self.velocity;

        if (self.target - next_val).abs() <= self.pos_eps && next_vel.abs() <= self.vel_eps {
            self.value = self.target;
            self.velocity = 0.0;
            self.running = false;
            false
        } else {
            self.value = next_val;
            self.velocity = next_vel;
            true
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl LogicalRect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    #[inline]
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }

    /// Bounds spring overshoot so the animated body never exceeds `max_px` past `target`
    /// (ported from DankMaterialShell `FluidGeometry.js::limitOvershoot`).
    pub fn limit_overshoot(self, target: Self, max_px: f32) -> Self {
        let cap = max_px.max(0.0);
        let left = self.x.clamp(target.x - cap, target.x + target.w + cap - 1.0);
        let top = self.y.clamp(target.y - cap, target.y + target.h + cap - 1.0);
        let right = (self.x + self.w).min(target.x + target.w + cap);
        let bottom = (self.y + self.h).min(target.y + target.h + cap);
        Self {
            x: left,
            y: top,
            w: (right - left).max(1.0),
            h: (bottom - top).max(1.0),
        }
    }

    /// Content reveal opacity derived from spatial expansion ratio (`FluidGeometry.js::contentOpacity`).
    pub fn content_opacity(self, target: Self) -> f32 {
        let rw = self.w / target.w.max(1.0);
        let rh = self.h / target.h.max(1.0);
        ((rw.min(rh) - 0.45) / 0.55).clamp(0.0, 1.0)
    }
}

/// 4D spatial spring for the morphing card geometry (`x, y, width, height`).
#[derive(Debug, Clone)]
pub struct RectSpring {
    pub x: Spring,
    pub y: Spring,
    pub w: Spring,
    pub h: Spring,
}

impl RectSpring {
    pub fn new(initial: LogicalRect, spec: SpringSpec) -> Self {
        Self {
            x: Spring::new(initial.x, spec).with_epsilon(0.08, 0.2),
            y: Spring::new(initial.y, spec).with_epsilon(0.08, 0.2),
            w: Spring::new(initial.w, spec).with_epsilon(0.08, 0.2),
            h: Spring::new(initial.h, spec).with_epsilon(0.08, 0.2),
        }
    }

    #[allow(dead_code)]
    pub fn snap_to(&mut self, r: LogicalRect) {
        self.x.snap_to(r.x);
        self.y.snap_to(r.y);
        self.w.snap_to(r.w);
        self.h.snap_to(r.h);
    }

    pub fn retarget(&mut self, r: LogicalRect) {
        self.x.retarget(r.x);
        self.y.retarget(r.y);
        self.w.retarget(r.w);
        self.h.retarget(r.h);
    }

    pub fn target(&self) -> LogicalRect {
        LogicalRect::new(self.x.target, self.y.target, self.w.target, self.h.target)
    }

    pub fn current(&self) -> LogicalRect {
        LogicalRect::new(
            self.x.value,
            self.y.value,
            self.w.value.max(1.0),
            self.h.value.max(1.0),
        )
        .limit_overshoot(self.target(), 18.0)
    }

    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        self.x.is_running() || self.y.is_running() || self.w.is_running() || self.h.is_running()
    }

    pub fn advance(&mut self, dt: f32) -> bool {
        let ax = self.x.advance(dt);
        let ay = self.y.advance(dt);
        let aw = self.w.advance(dt);
        let ah = self.h.advance(dt);
        ax || ay || aw || ah
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_frame_rate_invariance() {
        let mut s_60 = Spring::new(46.0, SpringSpec::DEFAULT_SPATIAL);
        let mut s_240 = Spring::new(46.0, SpringSpec::DEFAULT_SPATIAL);
        s_60.retarget(420.0);
        s_240.retarget(420.0);

        for _ in 0..15 {
            s_60.advance(1.0 / 60.0);
        }
        for _ in 0..60 {
            s_240.advance(1.0 / 240.0);
        }
        // Closed-form ODE guarantees identical position at t = 0.25s regardless of refresh rate!
        assert!(
            (s_60.value - s_240.value).abs() < 0.05,
            "60Hz={} vs 240Hz={}",
            s_60.value,
            s_240.value
        );
    }

    #[test]
    fn expressive_spring_has_subtle_mass_and_settles_cleanly() {
        let mut s = Spring::new(0.0, SpringSpec::DEFAULT_SPATIAL);
        s.retarget(100.0);
        let mut peak = 0.0_f32;
        for _ in 0..120 {
            s.advance(1.0 / 120.0);
            peak = peak.max(s.value);
        }
        assert!(!s.is_running());
        assert!((s.value - 100.0).abs() < 0.01);
        // Expressive spatial spring has gentle organic overshoot (~1%..8%), never wild bounce
        assert!(peak >= 100.2 && peak <= 108.0, "peak was {}", peak);
    }

    #[test]
    fn mid_flight_retarget_preserves_momentum() {
        let mut s = Spring::new(48.0, SpringSpec::SHEET_MORPH);
        s.retarget(240.0);
        for _ in 0..8 {
            s.advance(1.0 / 120.0);
        }
        let vel_before = s.velocity;
        assert!(vel_before > 100.0);
        s.retarget(420.0);
        assert_eq!(s.velocity, vel_before);
        for _ in 0..180 {
            s.advance(1.0 / 120.0);
        }
        assert!(!s.is_running());
        assert!((s.value - 420.0).abs() < 0.01);
    }
}
