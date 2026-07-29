//! Local collision avoidance — **ORCA** (Optimal Reciprocal Collision Avoidance,
//! van den Berg et al. 2011), the modern replacement for the position-nudge crowd
//! separation the hunters used to run (`world::separate_enemies`). Agents steer
//! smoothly *around* one another (and the player) by picking, each step, the
//! velocity closest to what they want that is guaranteed collision-free for a short
//! time horizon — instead of interpenetrating and being shoved apart after the fact.
//!
//! This is the pure math core: given one agent's state, its neighbours, and any
//! static disc obstacles, [`orca_velocity`] returns the velocity to move with this
//! step. It is planar (the XZ ground plane, mapped to [`glam::Vec2`] `x→x, z→y`) —
//! hunters live on the nav floor, so 2D is exact and cheap. The caller ([`crate`]'s
//! game side) supplies positions/velocities, integrates the result over `dt`, and
//! re-gates the step against the nav grid so avoidance never clips a wall or walks a
//! hunter off a ledge (avoidance decides *direction*, nav still owns *walkability*).
//!
//! Solver: each neighbour/obstacle contributes one **ORCA half-plane** (the set of
//! velocities that avoid it, with reciprocal agents each taking half the evasive
//! effort). The wanted velocity is projected into the intersection of all half-planes
//! (clamped to a max-speed disc) by a small 2-D linear program; in a dense pack where
//! no fully-safe velocity exists, a 3-D fallback picks the least-penetrating one.
//! Ported from the reference RVO2 `linearProgram2`/`linearProgram3`.

use glam::Vec2;

/// A moving disc agent that participates in reciprocal avoidance.
#[derive(Clone, Copy, Debug)]
pub struct Agent {
    /// Planar position (x, z).
    pub pos: Vec2,
    /// Current planar velocity — what the agent is actually moving at right now.
    /// Used for the reciprocal responsibility split (each of two agents assumes the
    /// other keeps its current velocity and shares the avoidance 50/50). Persist it
    /// across steps for smooth motion; zero on the first step is fine.
    pub vel: Vec2,
    /// The velocity the agent *wants* (toward its goal) — the solve returns the
    /// nearest collision-free velocity to this.
    pub pref_vel: Vec2,
    /// Disc radius (m).
    pub radius: f32,
    /// Speed cap (m/s): the solution is clamped to this disc. Give a holding agent a
    /// small floor so it can still yield ground to a packmate that needs to pass.
    pub max_speed: f32,
}

/// A disc the agent must avoid but that does not reciprocate — the player (hunters
/// don't collide with the player, but ORCA keeps them from piling onto its exact
/// cell) or any other non-agent hazard. The agent takes *full* responsibility for
/// avoiding it (unlike the 50/50 split between two agents).
#[derive(Clone, Copy, Debug)]
pub struct Obstacle {
    /// Planar position (x, z).
    pub pos: Vec2,
    /// Planar velocity (0 for a static disc; the player's motion for a moving one).
    pub vel: Vec2,
    /// Disc radius (m).
    pub radius: f32,
}

/// Tunable time horizons (seconds) for the avoidance look-ahead: how far in the
/// future a predicted collision must be for the agent to start steering around it.
/// Longer = smoother, earlier, wider avoidance; shorter = tighter, later, more
/// willing to pass close. `agents` is the reciprocal horizon, `obstacles` the
/// (usually shorter) one for non-reciprocal discs like the player.
#[derive(Clone, Copy, Debug)]
pub struct Horizons {
    pub agents: f32,
    pub obstacles: f32,
}

impl Default for Horizons {
    fn default() -> Self {
        // Tuned for tight indoor quarters + our small (~0.24 m) agent radius: react
        // early enough to weave, not so early that a whole room over-avoids.
        Self { agents: 1.5, obstacles: 1.0 }
    }
}

/// Numerical epsilon for the linear-program degeneracy checks (RVO2 `RVO_EPSILON`).
const EPS: f32 = 1e-5;

/// A directed line bounding a half-plane. The feasible side is to the **left** of
/// `dir` from `point`: velocities `v` with `det(dir, v − point) ≥ 0`.
#[derive(Clone, Copy)]
struct Line {
    point: Vec2,
    dir: Vec2,
}

/// 2×2 determinant / planar cross product.
#[inline]
fn det(a: Vec2, b: Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

/// Compute the safe velocity for `agent` this step: the velocity nearest its
/// `pref_vel` that avoids every neighbour (reciprocally) and obstacle (fully) for the
/// given [`Horizons`], clamped to `agent.max_speed`. `dt` is the sim step — used only
/// for the already-colliding fallback (recover over one step). Never panics; returns
/// a velocity with magnitude ≤ `max_speed`.
pub fn orca_velocity(
    agent: &Agent,
    neighbors: &[Agent],
    obstacles: &[Obstacle],
    horizons: Horizons,
    dt: f32,
) -> Vec2 {
    let inv_tau = 1.0 / horizons.agents.max(EPS);
    let inv_tau_obst = 1.0 / horizons.obstacles.max(EPS);
    let inv_dt = 1.0 / dt.max(EPS);

    let mut lines: Vec<Line> = Vec::with_capacity(neighbors.len() + obstacles.len());

    // Static / non-reciprocal discs first (full responsibility, factor 1.0). We do
    // NOT mark these as "hard" obstacle lines for the LP3 fallback (num_obst = 0
    // below): in a genuinely over-constrained crowd we'd rather a hunter momentarily
    // clip the player's disc than freeze, since it doesn't physically collide anyway.
    for o in obstacles {
        let rel_pos = o.pos - agent.pos;
        let rel_vel = agent.vel - o.vel;
        let combined = agent.radius + o.radius;
        lines.push(orca_line(rel_pos, rel_vel, combined, inv_tau_obst, inv_dt, agent.vel, 1.0));
    }
    // Reciprocal agents (each takes half, factor 0.5).
    for other in neighbors {
        let rel_pos = other.pos - agent.pos;
        let rel_vel = agent.vel - other.vel;
        let combined = agent.radius + other.radius;
        lines.push(orca_line(rel_pos, rel_vel, combined, inv_tau, inv_dt, agent.vel, 0.5));
    }

    let mut result = agent.pref_vel;
    let fail = linear_program2(&lines, agent.max_speed, agent.pref_vel, false, &mut result);
    if fail < lines.len() {
        linear_program3(&lines, 0, fail, agent.max_speed, &mut result);
    }
    result
}

/// Build one ORCA half-plane for a neighbour/obstacle. `rel_pos` = other − self,
/// `rel_vel` = self.vel − other.vel, `combined` = summed radii. `factor` is the share
/// of the avoidance this agent takes (0.5 reciprocal, 1.0 for a static obstacle).
/// Follows RVO2 `Agent::computeNewVelocity`: outside collision we project the relative
/// velocity onto the truncated velocity-obstacle cone (its cutoff circle or a leg);
/// already overlapping, we recover over one time step.
fn orca_line(
    rel_pos: Vec2,
    rel_vel: Vec2,
    combined: f32,
    inv_tau: f32,
    inv_dt: f32,
    agent_vel: Vec2,
    factor: f32,
) -> Line {
    let dist_sq = rel_pos.length_squared();
    let r_sq = combined * combined;
    let dir;
    let u;
    if dist_sq > r_sq {
        // No collision yet. `w` = relative velocity offset from the cone apex.
        let w = rel_vel - inv_tau * rel_pos;
        let w_len_sq = w.length_squared();
        let dot1 = w.dot(rel_pos);
        if dot1 < 0.0 && dot1 * dot1 > r_sq * w_len_sq {
            // Nearest point is on the cone's cutoff circle.
            let w_len = w_len_sq.sqrt().max(EPS);
            let unit_w = w / w_len;
            dir = Vec2::new(unit_w.y, -unit_w.x);
            u = (combined * inv_tau - w_len) * unit_w;
        } else {
            // Nearest point is on a cone leg; pick the side by the sign of the cross.
            let leg = (dist_sq - r_sq).max(0.0).sqrt();
            if det(rel_pos, w) > 0.0 {
                dir = Vec2::new(
                    rel_pos.x * leg - rel_pos.y * combined,
                    rel_pos.x * combined + rel_pos.y * leg,
                ) / dist_sq;
            } else {
                dir = -Vec2::new(
                    rel_pos.x * leg + rel_pos.y * combined,
                    -rel_pos.x * combined + rel_pos.y * leg,
                ) / dist_sq;
            }
            let dot2 = rel_vel.dot(dir);
            u = dot2 * dir - rel_vel;
        }
    } else {
        // Already interpenetrating — resolve over one step instead of the horizon.
        let w = rel_vel - inv_dt * rel_pos;
        let w_len = w.length();
        let unit_w = if w_len > EPS { w / w_len } else { Vec2::new(1.0, 0.0) };
        dir = Vec2::new(unit_w.y, -unit_w.x);
        u = (combined * inv_dt - w_len) * unit_w;
    }
    Line { point: agent_vel + factor * u, dir }
}

/// RVO2 `linearProgram1`: optimise along the single line `lines[i]`, subject to the
/// max-speed circle (`radius`) and every prior half-plane. Returns `false` if the
/// resulting feasible segment is empty (this line can't be satisfied). On success
/// writes the optimal point on the line into `result`.
fn linear_program1(
    lines: &[Line],
    i: usize,
    radius: f32,
    opt_vel: Vec2,
    dir_opt: bool,
    result: &mut Vec2,
) -> bool {
    let line = lines[i];
    let dot = line.point.dot(line.dir);
    let disc = dot * dot + radius * radius - line.point.length_squared();
    if disc < 0.0 {
        // The max-speed circle doesn't reach this line — infeasible.
        return false;
    }
    let sqrt_disc = disc.sqrt();
    let mut t_left = -dot - sqrt_disc;
    let mut t_right = -dot + sqrt_disc;
    for j in 0..i {
        let denom = det(line.dir, lines[j].dir);
        let numer = det(lines[j].dir, line.point - lines[j].point);
        if denom.abs() <= EPS {
            // Lines nearly parallel; if `line` is on the wrong side, infeasible.
            if numer < 0.0 {
                return false;
            }
            continue;
        }
        let t = numer / denom;
        if denom >= 0.0 {
            t_right = t_right.min(t);
        } else {
            t_left = t_left.max(t);
        }
        if t_left > t_right {
            return false;
        }
    }
    if dir_opt {
        // Optimising a direction (LP3 fallback): go as far as allowed along it.
        if opt_vel.dot(line.dir) > 0.0 {
            *result = line.point + t_right * line.dir;
        } else {
            *result = line.point + t_left * line.dir;
        }
    } else {
        // Optimising toward a point: clamp its projection onto the feasible segment.
        let t = line.dir.dot(opt_vel - line.point);
        *result = line.point + t.clamp(t_left, t_right) * line.dir;
    }
    true
}

/// RVO2 `linearProgram2`: find the velocity nearest `opt_vel` inside the max-speed
/// circle and all half-planes. Returns `lines.len()` on full success, or the index of
/// the first half-plane that couldn't be satisfied (the caller then runs LP3).
fn linear_program2(
    lines: &[Line],
    radius: f32,
    opt_vel: Vec2,
    dir_opt: bool,
    result: &mut Vec2,
) -> usize {
    if dir_opt {
        *result = opt_vel * radius;
    } else if opt_vel.length_squared() > radius * radius {
        *result = opt_vel.normalize_or_zero() * radius;
    } else {
        *result = opt_vel;
    }
    for i in 0..lines.len() {
        if det(lines[i].dir, lines[i].point - *result) > 0.0 {
            // `result` violates half-plane i — re-optimise constrained to line i.
            let temp = *result;
            if !linear_program1(lines, i, radius, opt_vel, dir_opt, result) {
                *result = temp;
                return i;
            }
        }
    }
    lines.len()
}

/// RVO2 `linearProgram3`: dense-crowd fallback when no fully-safe velocity exists.
/// Minimises the maximum half-plane penetration by, for each violated line, projecting
/// the others onto it and optimising along it. `num_obst` half-planes at the front are
/// treated as hard (never relaxed) — we pass 0, so nothing is hard.
fn linear_program3(lines: &[Line], num_obst: usize, begin: usize, radius: f32, result: &mut Vec2) {
    let mut distance = 0.0;
    for i in begin..lines.len() {
        if det(lines[i].dir, lines[i].point - *result) > distance {
            let mut proj: Vec<Line> = lines[..num_obst].to_vec();
            for j in num_obst..i {
                let determinant = det(lines[i].dir, lines[j].dir);
                let point = if determinant.abs() <= EPS {
                    if lines[i].dir.dot(lines[j].dir) > 0.0 {
                        continue; // same direction — no new constraint
                    }
                    0.5 * (lines[i].point + lines[j].point)
                } else {
                    lines[i].point
                        + (det(lines[j].dir, lines[i].point - lines[j].point) / determinant)
                            * lines[i].dir
                };
                let dir = (lines[j].dir - lines[i].dir).normalize_or_zero();
                proj.push(Line { point, dir });
            }
            let temp = *result;
            let opt = Vec2::new(-lines[i].dir.y, lines[i].dir.x);
            if linear_program2(&proj, radius, opt, true, result) < proj.len() {
                // Should not normally happen; keep the best-so-far result.
                *result = temp;
            }
            distance = det(lines[i].dir, lines[i].point - *result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(px: f32, pz: f32, vx: f32, vz: f32, pvx: f32, pvz: f32, max: f32) -> Agent {
        Agent {
            pos: Vec2::new(px, pz),
            vel: Vec2::new(vx, vz),
            pref_vel: Vec2::new(pvx, pvz),
            radius: 0.24,
            max_speed: max,
        }
    }

    /// With no neighbours and no obstacles the agent gets exactly its preferred
    /// velocity (avoidance is a no-op when nothing is in the way).
    #[test]
    fn free_agent_keeps_its_preferred_velocity() {
        let a = agent(0.0, 0.0, 0.0, 0.0, 3.0, 0.0, 4.0);
        let v = orca_velocity(&a, &[], &[], Horizons::default(), 1.0 / 60.0);
        assert!((v - a.pref_vel).length() < 1e-4, "free agent moves as preferred, got {v:?}");
    }

    /// The result never exceeds the max-speed cap, even when the preferred velocity
    /// asks for more.
    #[test]
    fn result_is_clamped_to_max_speed() {
        let a = agent(0.0, 0.0, 0.0, 0.0, 100.0, 0.0, 4.0);
        let v = orca_velocity(&a, &[], &[], Horizons::default(), 1.0 / 60.0);
        assert!(v.length() <= 4.0 + 1e-3, "clamped to max speed, got |v|={}", v.length());
    }

    /// Two agents on a head-on collision course must both deflect sideways — neither
    /// keeps driving straight into the other — and by symmetry to opposite sides
    /// (reciprocal 50/50). This is the core crowd behaviour separate_enemies faked.
    #[test]
    fn head_on_agents_deflect_to_opposite_sides() {
        // A at x=-2 heading +x; B at x=+2 heading -x. They'd collide at the origin.
        let a = agent(-2.0, 0.0, 3.0, 0.0, 3.0, 0.0, 4.0);
        let b = agent(2.0, 0.0, -3.0, 0.0, -3.0, 0.0, 4.0);
        let va = orca_velocity(&a, &[b], &[], Horizons::default(), 1.0 / 60.0);
        let vb = orca_velocity(&b, &[a], &[], Horizons::default(), 1.0 / 60.0);
        // Each gains a lateral (z) component to slip past.
        assert!(va.y.abs() > 0.2, "A deflects laterally, got {va:?}");
        assert!(vb.y.abs() > 0.2, "B deflects laterally, got {vb:?}");
        // Reciprocal → they pass on opposite sides (opposite lateral signs).
        assert!(va.y * vb.y < 0.0, "A and B swerve to opposite sides ({}, {})", va.y, vb.y);
        // Still make net forward progress (don't just stop dead).
        assert!(va.x > 0.5, "A still advances, got {va:?}");
        assert!(vb.x < -0.5, "B still advances, got {vb:?}");
    }

    /// A pair that would collide ends up with a reduced closing speed after applying
    /// the avoidance velocities — the whole point (they no longer drive straight in).
    #[test]
    fn avoidance_reduces_the_closing_speed() {
        let a = agent(-2.0, 0.0, 3.0, 0.0, 3.0, 0.0, 4.0);
        let b = agent(2.0, 0.0, -3.0, 0.0, -3.0, 0.0, 4.0);
        let va = orca_velocity(&a, &[b], &[], Horizons::default(), 1.0 / 60.0);
        let vb = orca_velocity(&b, &[a], &[], Horizons::default(), 1.0 / 60.0);
        // Closing speed along the line between them (x-axis): preferred was 6 m/s.
        let closing = va.x - vb.x; // A moves +x, B moves -x; closing = va.x - vb.x
        assert!(closing < 6.0, "closing speed drops below the head-on 6 m/s, got {closing}");
    }

    /// An agent heading straight at a static disc obstacle (the player) steers around
    /// it rather than driving into it.
    #[test]
    fn agent_avoids_a_static_obstacle_disc() {
        let a = agent(0.0, -2.0, 0.0, 3.0, 0.0, 3.0, 4.0);
        let player = Obstacle { pos: Vec2::new(0.0, 0.0), vel: Vec2::ZERO, radius: 0.3 };
        let v = orca_velocity(&a, &[], &[player], Horizons::default(), 1.0 / 60.0);
        assert!(v.x.abs() > 0.1, "agent sidesteps the obstacle, got {v:?}");
    }

    /// An agent already overlapping a neighbour is pushed apart (positive separation
    /// velocity along the line between them) — the collision-recovery branch.
    #[test]
    fn overlapping_agents_are_pushed_apart() {
        // Two agents 0.2 m apart with combined radius 0.48 → interpenetrating, both
        // trying to hold still (pref 0).
        let a = agent(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0);
        let b = agent(0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0);
        let va = orca_velocity(&a, &[b], &[], Horizons::default(), 1.0 / 60.0);
        // A should be pushed in −x (away from B at +x).
        assert!(va.x < -0.1, "overlapping agent is pushed away from its neighbour, got {va:?}");
    }
}
