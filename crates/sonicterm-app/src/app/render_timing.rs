use std::time::Instant;

pub struct RenderTiming {
    window: &'static str,
    start: Instant,
    last: Instant,
    parts: Vec<(&'static str, f32)>,
}

impl RenderTiming {
    pub fn start(window: &'static str) -> Option<Self> {
        if !tracing::enabled!(target: "render_timing", tracing::Level::DEBUG) {
            return None;
        }
        let now = Instant::now();
        Some(Self { window, start: now, last: now, parts: Vec::with_capacity(12) })
    }

    pub fn lap(&mut self, name: &'static str) {
        let now = Instant::now();
        let ms = now.saturating_duration_since(self.last).as_secs_f32() * 1000.0;
        self.parts.push((name, ms));
        self.last = now;
    }

    pub fn finish(mut self) {
        self.lap("tail");
        let total_ms = self.start.elapsed().as_secs_f32() * 1000.0;
        let mut line = format!("[render_timing] window={} total={total_ms:.2}ms", self.window);
        for (name, ms) in self.parts {
            line.push_str(&format!(" {name}={ms:.2}ms"));
        }
        tracing::debug!(target: "render_timing", %line);
    }
}
