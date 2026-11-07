/// A damped spring that moves `value` toward `target`.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
}

impl Spring {
    pub const fn new(value: f32) -> Self {
        Self { value, velocity: 0.0, target: value }
    }

    /// Advances the spring by `dt` seconds. Returns true while it is still moving.
    pub fn step(&mut self, dt: f32, stiffness: f32, damping: f32) -> bool {
        // Small fixed substeps keep the integration stable at any frame rate.
        let steps = (dt / 0.004).ceil().max(1.0) as i32;
        let h = dt / steps as f32;
        for _ in 0..steps {
            let force = -stiffness * (self.value - self.target) - damping * self.velocity;
            self.velocity += force * h;
            self.value += self.velocity * h;
        }
        if (self.value - self.target).abs() < 0.0005 && self.velocity.abs() < 0.005 {
            self.value = self.target;
            self.velocity = 0.0;
            false
        } else {
            true
        }
    }
}
