//! Fixed-timestep + frame-pacing clock for a winit render loop — the reusable
//! timing skeleton a game's `ApplicationHandler` drives its update/render from.
//!
//! Two independent rates:
//! - **Simulation** advances in discrete `fixed_dt` steps so physics/movement
//!   are frame-rate independent. Per rendered frame, [`FrameClock::begin_frame`]
//!   accumulates the real elapsed time and [`FrameClock::take_fixed_steps`]
//!   reports how many fixed steps to run (capped, with backlog drop after a
//!   stall so a hitch can't spiral into an ever-growing catch-up).
//! - **Rendering** is paced to a max FPS via [`FrameClock::pace`], so the loop
//!   sleeps between frames instead of burning the GPU at thousands of fps.

use std::time::{Duration, Instant};

/// A render-loop clock: real-dt tracking, a fixed-timestep accumulator, and a
/// frame-budget pacer. Construct once, drive from the winit handlers.
pub struct FrameClock {
    fixed_dt: f32,
    max_substeps: u32,
    frame_budget: Duration,
    last_frame: Option<Instant>,
    /// Elapsed time not yet consumed by a fixed sim step.
    accumulator: f32,
    /// Next instant we're allowed to render (the FPS cap deadline).
    next_frame: Option<Instant>,
}

impl FrameClock {
    /// `fixed_hz` — simulation rate (e.g. 120.0). `max_substeps` — cap on fixed
    /// steps per frame (backlog beyond this is dropped). `max_fps` — render pacing
    /// ceiling (e.g. 240).
    pub fn new(fixed_hz: f32, max_substeps: u32, max_fps: u32) -> Self {
        FrameClock {
            fixed_dt: 1.0 / fixed_hz,
            max_substeps,
            frame_budget: Duration::from_micros(1_000_000 / max_fps as u64),
            last_frame: None,
            accumulator: 0.0,
            next_frame: None,
        }
    }

    /// The fixed simulation timestep, in seconds — pass this to each sim step.
    pub fn fixed_dt(&self) -> f32 {
        self.fixed_dt
    }

    /// Call once at the start of a rendered frame. Returns the real elapsed time
    /// since the previous frame (clamped to 0.1 s so a breakpoint/stall doesn't
    /// inject a huge dt) and folds it into the fixed-step accumulator.
    pub fn begin_frame(&mut self, now: Instant) -> f32 {
        let dt = self
            .last_frame
            .replace(now)
            .map(|t| (now - t).as_secs_f32())
            .unwrap_or(1.0 / 60.0)
            .min(0.1);
        self.accumulator += dt;
        dt
    }

    /// How many fixed sim steps to run this frame, draining the accumulator. Run
    /// exactly this many `world.fixed_step(clock.fixed_dt())` calls. Caps at
    /// `max_substeps`; if it hits the cap the remaining backlog is dropped.
    pub fn take_fixed_steps(&mut self) -> u32 {
        let mut steps = 0;
        while self.accumulator >= self.fixed_dt && steps < self.max_substeps {
            self.accumulator -= self.fixed_dt;
            steps += 1;
        }
        if steps == self.max_substeps {
            self.accumulator = 0.0; // drop backlog after a stall
        }
        steps
    }

    /// Frame pacing, driven from `about_to_wait`. Returns `(should_redraw,
    /// wait_until)`: request a redraw iff `should_redraw`, then set the winit
    /// control flow to `WaitUntil(wait_until)`. Advances from the deadline (not
    /// `now`) for steady pacing, resyncing if we've fallen far behind.
    pub fn pace(&mut self, now: Instant) -> (bool, Instant) {
        let next = self.next_frame.get_or_insert(now);
        let redraw = now >= *next;
        if redraw {
            *next += self.frame_budget;
            if *next < now {
                *next = now + self.frame_budget;
            }
        }
        (redraw, *next)
    }
}
