//! Damped spring with SwiftUI-style parameters.
//!
//! `response` is the period of the undamped oscillation in seconds and
//! `damping` the damping ratio (1 = critically damped). The same formula is
//! available on macOS as `Spring(response:dampingRatio:)`, which keeps the
//! two platforms' motion identical for the same settings.

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
}

impl Spring {
    pub const fn at(value: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
        }
    }

    pub fn step(&mut self, dt: f32, response: f32, damping: f32) {
        let omega = std::f32::consts::TAU / response.max(0.01);
        let stiffness = omega * omega;
        let friction = 2.0 * damping * omega;
        // Semi-implicit Euler in fixed sub-steps stays stable for stiff
        // springs and long frames (e.g. after the display wakes up).
        let steps = (dt / (1.0 / 600.0)).ceil().clamp(1.0, 120.0);
        let h = dt / steps;
        for _ in 0..steps as u32 {
            let acceleration = -stiffness * (self.value - self.target) - friction * self.velocity;
            self.velocity += acceleration * h;
            self.value += self.velocity * h;
        }
    }

    /// True once the spring is within `epsilon` of its target and nearly still.
    pub fn is_settled(&self, epsilon: f32) -> bool {
        (self.value - self.target).abs() < epsilon && self.velocity.abs() < epsilon * 10.0
    }
}

#[cfg(test)]
mod tests {
    use super::Spring;

    #[test]
    fn spring_settles_on_target() {
        let mut spring = Spring::at(0.0);
        spring.target = 10.0;
        for _ in 0..240 {
            spring.step(1.0 / 120.0, 0.3, 0.8);
        }
        assert!(spring.is_settled(0.01), "{spring:?}");
    }

    #[test]
    fn underdamped_spring_overshoots() {
        let mut spring = Spring::at(0.0);
        spring.target = 1.0;
        let mut peak = 0.0_f32;
        for _ in 0..120 {
            spring.step(1.0 / 120.0, 0.3, 0.5);
            peak = peak.max(spring.value);
        }
        assert!(peak > 1.05, "peak {peak}");
    }

    #[test]
    fn critically_damped_spring_does_not_overshoot() {
        let mut spring = Spring::at(0.0);
        spring.target = 1.0;
        for _ in 0..240 {
            spring.step(1.0 / 120.0, 0.3, 1.0);
            assert!(spring.value <= 1.0 + 1e-4, "{spring:?}");
        }
    }
}
