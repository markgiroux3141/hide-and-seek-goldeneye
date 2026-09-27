//! `chraction.c` / `chr.c` — what a chr's body does each tick, and the geometry
//! queries the bot brain asks of it.

use glam::{Vec2, Vec3};

use super::bot;
use super::chr::{Act, Chr, GoPos, HAND_LEFT, HAND_RIGHT};
use super::pd_nav;
use super::pdmath::{baddtor, dtor, turn, M_BADPI};
use super::sim::{Shot, Sim};
use super::tile_level::{CdResult, PerimCyl, TileFlag, TileLevel};
use super::thirdperson::{self, AttackAnimConfig};
use super::weapons;

/// `g_HeadAnims[HEADANIM_MOVING].translateperframe` — computed by `bhead_reset`
/// (`bondheadreset.c:90`) from `ANIM_0029`'s root Z motion over frames 7..16:
/// `1790 * 0.1 / 9.5`. Measured with `pd_anim.py`; it is the unit of the shove.
pub const TRANSLATE_PER_FRAME_MOVING: f32 = 18.842_107;

/// `c_scalex`/`c_scaley` for one NTSC player at 320×220 with fovy 60:
/// `tan(30°) / 110`. `bgun_calculate_bot_shot_spread` reads the *human* player's
/// camera; this is its single-player value.
pub const C_SCALE: f32 = 0.005_248_7;
pub const FOVY: f32 = 60.0;

// ─── chr_tick ────────────────────────────────────────────────────────────────

/// `chr_tick` for a bot: action tick, animation + position, aim tween, flinch,
/// then the shots whose triggers `bot_tick_unpaused` pulled.
pub fn chr_tick(sim: &mut Sim, i: usize) {
    let g = sim.g;
    {
        let a = &mut sim.chrs[i].aibot;
        if a.fadeintimer60 > 0 {
            if a.fadeintimer60 > g.lvupdate60 {
                a.fadeintimer60 -= g.lvupdate60;
            } else {
                a.fadeintimer60 = 0;
            }
        }
    }
    match sim.chrs[i].actiontype {
        Act::GoPos => chr_tick_gopos(sim, i),
        Act::Die => chr_tick_die(sim, i),
        Act::Dead => chr_tick_dead(sim, i),
        Act::Stand => {}
    }
    // ACT_DEAD has no animation update.
    if sim.chrs[i].actiontype != Act::Dead {
        sim.chrs[i].model.tick(g.lvupdate240);
        chr_update_position(sim, i);
    }
    chr_tween_aim(&mut sim.chrs[i], g.lvupdate60f, g.lvupdate60);
    // Flinch counter (`chr.c:2725`). PD only advances it while the chr is on screen;
    // the arena is always in view, so it always advances here.
    {
        let c = &mut sim.chrs[i];
        if c.flinchcnt >= 0 {
            c.flinchcnt += g.lvupdate60;
            if c.flinchcnt >= 30 {
                c.flinchcnt = -1;
            }
        }
    }
    chr_tick_shots(sim, i);
}

/// `chr_update_position`, bot branch (`chr.c:521-1023`, the non-ladder,
/// non-jump, non-player path): the animation's own horizontal travel is replaced
/// by `bot_update_lateral`, then the shove and any fall drift, then
/// `chr_calculate_push_pos`, then the ground: step up by low-pass, drop by gravity.
///
/// `chr.pos.y` is `chr->manground`; `prop->pos.y` is that plus the clip's root
/// height ([`Chr::prop_pos`]).
///
/// Ladders and the duck/crouch heights (from the go-to's pad flags and from the
/// tiles) are ported; the body then plays the duck/squat rows. Not ported: lifts and the NTSC
/// `forceslowupdates` guard that freezes an *off-screen* bot about to fall out of
/// the world at a low frame rate (a lag fix, not behaviour).
fn chr_update_position(sim: &mut Sim, i: usize) {
    let g = sim.g;
    let freal = g.lvupdate60freal;
    let cyls = perims_except(sim, i);
    let level = &sim.level;
    let c = &mut sim.chrs[i];
    let prev = c.pos;
    c.prevpos = prev;
    let manground = prev.y;
    // prop->pos before the move, and arg2 (the new root position) — `chr.c:573`.
    let prop = c.prop_pos();
    let mv = if g.lvupdate240 > 0 { bot::bot_update_lateral(c, g.lvframe60, g.lvupdate240, freal) } else { Vec2::ZERO };
    let mut arg2 = Vec3::new(prev.x + mv.x, prop.y, prev.z + mv.y);

    // `chr.c:618`: a chr on a go-to touching a ladder climbs it.
    c.onladder = c.actiontype == Act::GoPos
        && level.cd_find_ladder(prop, c.radius * 2.5, manground + c.height - prop.y, manground + 1.0 - prop.y).is_some();
    // `chr.c:628`: a bot's height, from the duck/crouch tiles around it.
    c.height = 185.0;
    if c.actiontype == Act::GoPos && c.act_gopos.duck {
        c.height = 135.0;
    } else if c.actiontype == Act::GoPos && c.act_gopos.crouch {
        c.height = 90.0;
    } else if level.is_cyl_touching_tile_with_flags(TileFlag::Duck, prop, c.radius * 1.1, manground + 185.0 - prop.y, manground - 10.0 - prop.y) {
        c.height = 135.0;
    } else if level.is_cyl_touching_tile_with_flags(TileFlag::Crouch, prop, c.radius * 1.1, manground + 135.0 - prop.y, manground - 10.0 - prop.y) {
        c.height = 90.0;
    }
    bmove_dampen_shotspeed(&mut c.aibot.shotspeed, freal);
    arg2.x += c.aibot.shotspeed.x * TRANSLATE_PER_FRAME_MOVING * freal * 0.5;
    arg2.z += c.aibot.shotspeed.z * TRANSLATE_PER_FRAME_MOVING * freal * 0.5;
    arg2.x += c.fallspeed.x * freal;
    arg2.z += c.fallspeed.z * freal;

    // `chr.c:753`: on a ladder the whole lateral move becomes climb (≤ 100 cm).
    let mut yincrement = 0.0;
    if c.onladder {
        let (xdiff, zdiff) = (arg2.x - prev.x, arg2.z - prev.z);
        arg2.x = prev.x;
        arg2.z = prev.z;
        yincrement += (xdiff * xdiff + zdiff * zdiff).sqrt().min(100.0);
    }

    chr_calculate_push_pos(c, level, prop, &mut arg2, &cyls, g.lvframe60);

    if c.onladder {
        // `chr.c:806`: climb if the cylinder fits; the ground is wherever it is.
        let mut m = manground;
        if chr_ascend(c, level, prop, arg2, yincrement, &cyls) {
            m += yincrement;
        }
        c.sumground = m * 9.999_998;
        c.ground = m;
        c.pos = Vec3::new(arg2.x, m, arg2.z);
        return;
    }

    // Ground: probe from 69 above manground, which is the step height (`chr.c:830`).
    let probe = if arg2.y - manground < 69.0 { Vec3::new(arg2.x, manground + 69.0, arg2.z) } else { arg2 };
    let (mut ground, floorpoly) = level.cd_find_ground_at_cyl(probe, c.radius);
    if ground < -100_000.0 {
        ground = -100_000.0;
    }
    c.ground = ground;
    c.floorroom = floorpoly.and_then(|p| level.geom.polys[p].room);

    let mut m = manground;
    let mut die = false;
    if c.fallspeed.y != 0.0 || c.ground < m {
        if m <= -30_000.0 {
            die = true;
        }
        let mut fallspeed = c.fallspeed.y;
        projectile_update_fall(&mut yincrement, &mut fallspeed, freal);
        if chr_ascend(c, level, prop, arg2, yincrement, &cyls) {
            m += yincrement;
            c.fallspeed.y = fallspeed;
        }
        if m <= c.ground {
            m = c.ground;
            c.sumground = c.ground * 9.999_998;
            c.fallspeed.y = 0.0;
        }
    } else if m <= c.ground {
        for _ in 0..g.lvupdate60 {
            c.sumground = c.sumground * 0.9 + c.ground;
            c.fallspeed.x *= 0.9;
            c.fallspeed.z *= 0.9;
        }
        m = c.sumground * 0.100_000_024;
        if m < c.ground - 30.0 {
            m = c.ground - 30.0;
            c.sumground = (c.ground - 30.0) * 9.999_998;
        }
        if c.fallspeed.x.abs() < 0.1 && c.fallspeed.z.abs() < 0.1 {
            c.fallspeed.x = 0.0;
            c.fallspeed.z = 0.0;
        }
    }
    c.pos = Vec3::new(arg2.x, m, arg2.z);
    if die {
        chr_die(sim, i, None);
    }
}

/// Every other chr's perimeter cylinder, as `CDTYPE_ALL` collision sees it
/// (`chr_get_geometry`, `chr.c:4949`): dead chrs have none, and a dying chr's is
/// `GEOFLAG_BLOCK_SHOOT` only, so neither blocks movement.
pub fn perims_except(sim: &Sim, i: usize) -> Vec<PerimCyl> {
    sim.chrs
        .iter()
        .enumerate()
        .filter(|(j, o)| *j != i && !o.is_dead())
        .map(|(_, o)| PerimCyl { x: o.pos.x, z: o.pos.z, radius: o.radius, ymin: o.pos.y, ymax: o.pos.y + o.height })
        .collect()
}

/// `chr_get_bbox` (`chr.c:4995`) relative to `prop->pos.y`, the way every caller
/// passes it: `(ymax - prop->pos.y, ymin - prop->pos.y)`. The cylinder starts
/// 20 cm above `manground`, so anything lower is stepped over.
fn chr_bbox_rel(c: &Chr, prop_y: f32) -> (f32, f32) {
    (c.pos.y + c.height - prop_y, c.pos.y + 20.0 - prop_y)
}

/// `chr_calculate_push_pos` (`chr.c:204`): try the move; if something is in the
/// way, slide along the edge that was hit (method 1), else round that edge's
/// nearer end (method 2), else stay put. Moves larger than half the radius on
/// either axis are swept first.
fn chr_calculate_push_pos(c: &mut Chr, level: &TileLevel, prop: Vec3, dst: &mut Vec3, cyls: &[PerimCyl], lvframe60: i32) {
    let radius = c.radius;
    let halfradius = radius * 0.5;
    let (ymax, ymin) = chr_bbox_rel(c, prop.y);
    let big = |to: Vec3| {
        let (mx, mz) = (to.x - prop.x, to.z - prop.z);
        mx > halfradius || mz > halfradius || mx < -halfradius || mz < -halfradius
    };
    let (cdresult, edge) = if big(*dst) {
        match level.cd_test_cylmove_oobfail_findclosest(prop, *dst, radius, ymax, ymin, cyls) {
            (CdResult::NoCollision, _) => level.cd_test_volume_closestedge(prop, *dst, radius, ymax, ymin, cyls),
            other => other,
        }
    } else {
        level.cd_test_volume_closestedge(prop, *dst, radius, ymax, ymin, cyls)
    };
    // The re-test each candidate gets (`chr.c:354-380`).
    let clear = |sp44: Vec3| -> bool {
        let r = if big(sp44) {
            match level.cd_test_cylmove_oobfail(prop, sp44, radius, ymax, ymin, cyls) {
                CdResult::NoCollision => level.cd_test_volume_simple(sp44, radius, ymax, ymin, cyls),
                r => r,
            }
        } else {
            level.cd_test_volume_simple(sp44, radius, ymax, ymin, cyls)
        };
        r == CdResult::NoCollision
    };
    let mut moveok = false;
    match cdresult {
        CdResult::Error => {}
        CdResult::NoCollision => {
            c.invalidmove = 0;
            c.lastmoveok60 = lvframe60;
            moveok = true;
        }
        CdResult::Collision => {
            let (sp78, sp6c) = edge.unwrap_or((prop, prop));
            let sp60 = Vec2::new(dst.x - prop.x, dst.z - prop.z);
            // Method 1: project the move onto the edge.
            if sp78.x != sp6c.x || sp78.z != sp6c.z {
                let sp54 = Vec2::new(sp6c.x - sp78.x, sp6c.z - sp78.z).normalize();
                let value = sp60.dot(sp54);
                let sp44 = Vec3::new(sp54.x * value + prop.x, dst.y, sp54.y * value + prop.z);
                if clear(sp44) {
                    dst.x = sp44.x;
                    dst.z = sp44.z;
                    c.invalidmove = 2;
                    moveok = true;
                }
            }
            // Method 2: if the destination is within a radius of one of the edge's
            // ends, project the move onto the perpendicular to (end − pos).
            if !moveok {
                let corner = |vtx: Vec3| -> Option<Vec3> {
                    if vtx.x == prop.x && vtx.z == prop.z {
                        return None;
                    }
                    let sp54 = Vec2::new(-(vtx.z - prop.z), vtx.x - prop.x).normalize();
                    let value = sp60.dot(sp54);
                    Some(Vec3::new(sp54.x * value + prop.x, dst.y, sp54.y * value + prop.z))
                };
                let near = |vtx: Vec3| Vec2::new(vtx.x - dst.x, vtx.z - dst.z).length_squared() <= radius * radius;
                let cand = if near(sp78) { corner(sp78) } else if near(sp6c) { corner(sp6c) } else { None };
                if let Some(sp44) = cand {
                    if clear(sp44) {
                        dst.x = sp44.x;
                        dst.z = sp44.z;
                        c.invalidmove = 2;
                        moveok = true;
                    }
                }
            }
        }
    }
    if !moveok {
        dst.x = prop.x;
        dst.z = prop.z;
        c.invalidmove = 1;
    }
}

/// `chr_ascend` (`chr.c:486`): may the chr's cylinder move `amount` vertically
/// from `pos` without meeting a wall?
fn chr_ascend(c: &Chr, level: &TileLevel, prop: Vec3, pos: Vec3, amount: f32, cyls: &[PerimCyl]) -> bool {
    let (ymax, ymin) = chr_bbox_rel(c, prop.y);
    level.cd_test_volume_simple(pos + Vec3::Y * amount, c.radius, ymax, ymin, cyls) == CdResult::NoCollision
}

/// `chr_adjust_pos_for_spawn(radius, pos, rooms, angle, allowonscreen = true,
/// force = false, onlysurrounding = false)` (`chraction.c:15018`), as
/// `player_choose_spawn_location` calls it (`player.c:344`): the pad itself if a
/// cylinder there reaching 2 m above and below is clear of walls and chrs, else the
/// first of 8 points 60 cm around it (starting at `angle`, 45° apart) that the pad
/// can see and that is clear the same way. `pos` is the pad's own position, not
/// the ground; the caller stands the chr on the floor afterwards.
pub fn chr_adjust_pos_for_spawn(level: &TileLevel, chrradius: f32, pos: Vec3, angle: f32, cyls: &[PerimCyl]) -> Option<Vec3> {
    let ymax = 200.0;
    let ymin_at = |p: Vec3| {
        let ground = level.cd_find_ground_at_cyl(p, chrradius).0;
        if ground > -100_000.0 && ground - p.y < -200.0 {
            ground - p.y
        } else {
            -200.0
        }
    };
    if level.cd_test_volume_simple(pos, chrradius, ymax, ymin_at(pos), cyls) != CdResult::Collision {
        return Some(pos);
    }
    let mut curangle = angle;
    for _ in 0..8 {
        let testpos = Vec3::new(pos.x + curangle.sin() * 60.0, pos.y, pos.z + curangle.cos() * 60.0);
        if level.los_autoflags(pos, testpos)
            && level.cd_test_volume_simple(testpos, chrradius, ymax, ymin_at(testpos), cyls) != CdResult::Collision
        {
            return Some(testpos);
        }
        curangle += baddtor(45.0);
        if curangle >= baddtor(360.0) {
            curangle -= baddtor(360.0);
        }
    }
    None
}

/// `projectile_update_fall` (`projectile.c:34`): gravity 0.2778 cm/tick².
pub fn projectile_update_fall(yincrement: &mut f32, speed: &mut f32, lvupdate60: f32) {
    let s = *speed - lvupdate60 * 0.277_777_79;
    *yincrement += lvupdate60 * (*speed + s) * 0.5;
    *speed = s;
}

/// `chr_prop_can_move_to_pos_without_nav` (`chraction.c:5173`): a clear swept
/// line to `topos`, and two more offset `turndist` to either side (room to turn).
pub fn chr_prop_can_move_to_pos_without_nav(c: &Chr, level: &TileLevel, topos: Vec3, turndist: f32, cyls: &[PerimCyl]) -> bool {
    let frompos = c.prop_pos();
    let (ymax, ymin) = chr_bbox_rel(c, frompos.y);
    if level.cd_test_cylmove_oobok(frompos, topos, ymax, ymin, cyls) == CdResult::Collision {
        return false;
    }
    let d = Vec2::new(topos.x - frompos.x, topos.z - frompos.z);
    if d == Vec2::ZERO {
        return true;
    }
    let d = d.normalize();
    let (tx, tz) = (d.x * turndist, d.y * turndist);
    for side in [1.0f32, -1.0] {
        let nf = Vec3::new(frompos.x + tz * side, frompos.y, frompos.z - tx * side);
        let nt = Vec3::new(topos.x + tz * side, topos.y, topos.z - tx * side);
        if level.cd_test_cylmove_oobok(frompos, nf, ymax, ymin, cyls) == CdResult::Collision
            || level.cd_test_cylmove_oobok(nf, nt, ymax, ymin, cyls) == CdResult::Collision
        {
            return false;
        }
    }
    true
}

/// `bmove_dampen_shotspeed` (`bondmove.c:1841`).
pub fn bmove_dampen_shotspeed(s: &mut Vec3, lvupdate60freal: f32) {
    if s.x != 0.0 || s.z != 0.0 {
        let mut hyp = (s.x * s.x + s.z * s.z).sqrt();
        if hyp > 1.5 {
            s.x *= 1.5 / hyp;
            s.z *= 1.5 / hyp;
            hyp = 1.5;
        }
        for k in 0..3 {
            let v = &mut s[k];
            if hyp > 0.0001 {
                if *v > 0.0 {
                    *v -= (1.0 / 30.0) * lvupdate60freal * *v / hyp;
                    if *v < 0.0 {
                        *v = 0.0;
                    }
                } else if *v < 0.0 {
                    *v -= (1.0 / 30.0) * lvupdate60freal * *v / hyp;
                    if *v > 0.0 {
                        *v = 0.0;
                    }
                }
            } else {
                *v = 0.0;
            }
        }
    }
}

// ─── Go-to ───────────────────────────────────────────────────────────────────

/// The rooms a chr is in, as PD's `prop->rooms` collapses to for a chr on a floor
/// (`chr_update_position` keeps just `floorroom` when it's among them).
pub fn chr_rooms(sim: &Sim, i: usize) -> Vec<u16> {
    let c = &sim.chrs[i];
    c.floorroom.or_else(|| sim.level.floor_room(c.pos, c.radius)).into_iter().collect()
}

/// `chr_go_to_room_pos` (`chraction.c:6124`), non-magic: the waypoint nearest the
/// chr and the one nearest `pos` (`waypoint_find_closest_to_pos`), then
/// `nav_find_route` under the chr's nav seed into the 6-slot array. PD starts the
/// go-to when the count is > 1, and that count includes the NULL terminator
/// (`padhalllv.c:668`), so it means **at least one** waypoint.
pub fn chr_go_to_room_pos(sim: &mut Sim, i: usize, pos: Vec3) -> bool {
    if sim.chrs[i].is_dead() {
        return false;
    }
    let rooms = chr_rooms(sim, i);
    // SUBSTITUTION: PD is handed the destination's rooms; the floor room under it.
    let endrooms: Vec<u16> = sim.level.floor_room(pos, 20.0).into_iter().collect();
    let prop = sim.chrs[i].prop_pos();
    sim.stats.goto_log.push((prop, rooms.clone(), pos));
    let next = sim.nav.waypoint_find_closest_to_pos(&sim.level, prop, &rooms);
    let last = sim.nav.waypoint_find_closest_to_pos(&sim.level, pos, &endrooms);
    sim.stats.gotos += 1;
    if next.is_none() {
        sim.stats.goto_no_start += 1;
    } else if last.is_none() {
        sim.stats.goto_no_end += 1;
    }
    let (route, numwaypoints) = match (next, last) {
        (Some(a), Some(b)) => {
            let seed = pd_nav::chrnavseed(sim.g.lvframe60, i);
            sim.nav.nav_find_route(a, b, pd_nav::MAX_CHRWAYPOINTS, seed, &mut sim.rng)
        }
        _ => (Vec::new(), 0),
    };
    if next.is_some() && last.is_some() && numwaypoints <= 1 {
        sim.stats.goto_no_route += 1;
    }
    if numwaypoints > 1 {
        let age = (sim.rng.random() % 100) as i32;
        let c = &mut sim.chrs[i];
        c.actiontype = Act::GoPos;
        c.act_gopos = GoPos { endpos: pos, waypoints: route, curindex: 0, target: last, init: true, crouch: false, duck: false, age };
        return true;
    }
    false
}

/// `chr_gopos_advance_waypoint` (`chraction.c:5557`): step to the next loaded
/// waypoint, or, once past the third slot, reload the route from the current
/// waypoint to `target` under the chr's nav seed and continue from slot 1.
fn chr_gopos_advance_waypoint(sim: &mut Sim, i: usize) {
    let gp = &sim.chrs[i].act_gopos;
    if gp.curindex < 3 {
        sim.chrs[i].act_gopos.curindex += 1;
    } else {
        let from = gp.waypoints.get(gp.curindex).copied();
        let target = gp.target;
        sim.chrs[i].act_gopos.curindex = 1;
        if let (Some(from), Some(target)) = (from, target) {
            let seed = pd_nav::chrnavseed(sim.g.lvframe60, i);
            let (route, _) = sim.nav.nav_find_route(from, target, pd_nav::MAX_CHRWAYPOINTS, seed, &mut sim.rng);
            sim.chrs[i].act_gopos.waypoints = route;
        }
    }
}

/// `chr_run_from_pos` (`chraction.c:15690`), **with PD's bug**: the flee vector is
/// never added back to the chr's position, so the destination is the point
/// `away * rundist` in world coordinates, cut at the first wall on the way there.
pub fn chr_run_from_pos(sim: &mut Sim, i: usize, rundist: f32, frompos: Vec3) -> bool {
    let c = &sim.chrs[i];
    if c.is_dead() {
        return false;
    }
    let pp = c.prop_pos();
    let mut delta = Vec3::new(pp.x - frompos.x, pp.y, pp.z - frompos.z);
    if delta.x == 0.0 || delta.z == 0.0 {
        return false;
    }
    let cur = (delta.x * delta.x + delta.z * delta.z).sqrt();
    delta.x *= rundist / cur;
    delta.z *= rundist / cur;
    let dir = delta - pp;
    if let Some(hit) = sim.level.raycast_shoot(pp, dir, dir.length()) {
        delta = hit.point;
    }
    chr_go_to_room_pos(sim, i, delta)
}

/// `chr_try_stop` → `chr_stop` → `chr_stand_immediate`: back to `ACT_STAND`.
pub fn chr_try_stop(sim: &mut Sim, i: usize) -> bool {
    let c = &mut sim.chrs[i];
    if c.is_dead() {
        return false;
    }
    c.actiontype = Act::Stand;
    true
}

/// `pos_is_moving_towards_pos_or_stopped_in_range` (`chraction.c:11646`).
fn moving_towards_or_stopped_in_range(prev: Vec3, moved: Vec3, target: Vec3, range: f32) -> bool {
    let pd = Vec2::new(target.x - prev.x, target.z - prev.z);
    if moved.x == 0.0 && moved.z == 0.0 {
        return pd.length_squared() <= range * range;
    }
    let tmp = moved.x * pd.x + moved.z * pd.y;
    if tmp > 0.0 {
        let sqmoved = moved.x * moved.x + moved.z * moved.z;
        let sqprev = pd.length_squared();
        return (sqprev - range * range) * sqmoved <= tmp * tmp;
    }
    false
}

/// `pos_is_arriving_laterally_at_pos` (`chraction.c:11682`).
pub fn pos_is_arriving_laterally_at_pos(prev: Vec3, cur: Vec3, target: Vec3, range: f32) -> bool {
    if prev.x <= target.x - range && cur.x <= target.x - range {
        return false;
    }
    if prev.x >= target.x + range && cur.x >= target.x + range {
        return false;
    }
    if prev.z <= target.z - range && cur.z <= target.z - range {
        return false;
    }
    if prev.z >= target.z + range && cur.z >= target.z + range {
        return false;
    }
    moving_towards_or_stopped_in_range(prev, Vec3::new(cur.x - prev.x, 0.0, cur.z - prev.z), target, range)
}

/// `pos_is_arriving_at_pos` (`chraction.c:11716`): arriving laterally, and within
/// 150 cm vertically.
pub fn pos_is_arriving_at_pos(prev: Vec3, cur: Vec3, target: Vec3, range: f32) -> bool {
    if prev.y <= target.y - 150.0 && cur.y <= target.y - 150.0 {
        return false;
    }
    if prev.y >= target.y + 150.0 && cur.y >= target.y + 150.0 {
        return false;
    }
    pos_is_arriving_laterally_at_pos(prev, cur, target, range)
}

/// `chr_tick_gopos` (`chraction.c:12800`), non-magic branch (magic is off in MP).
/// Arrival is tested from `prop->pos`. Steering to the current point
/// (`chr_nav_tick_main`) is a straight line with `roty` snapped to it, which is
/// what `chr_turn_toward` does for a bot (`chraction.c:11481`); its "expensive"
/// obstacle-stepping mode is not ported. Lifts and `PADFLAG_AIIGNOREY` are not
/// ported (no Complex waypoint uses them).
fn chr_tick_gopos(sim: &mut Sim, i: usize) {
    let g = sim.g;
    sim.chrs[i].act_gopos.age += 1;

    // Stuck for a second: re-route to the same destination.
    if sim.chrs[i].lastmoveok60 < g.lvframe60 - 60 {
        let end = sim.chrs[i].act_gopos.endpos;
        sim.chrs[i].lastmoveok60 = g.lvframe60;
        sim.stats.repaths += 1;
        {
            let c = &sim.chrs[i];
            let gp = &c.act_gopos;
            let aim = gp.waypoints.get(gp.curindex).map_or(gp.endpos, |&w| sim.nav.waypoint_pos(w));
            sim.stats.repath_at.push((c.pos, aim));
        }
        if !chr_go_to_room_pos(sim, i, end) {
            chr_try_stop(sim, i);
            return;
        }
    }

    let pad_of = |sim: &Sim, k: usize| -> Option<(Vec3, pd_nav::PadFlags)> {
        let gp = &sim.chrs[i].act_gopos;
        gp.waypoints.get(k).map(|&w| {
            let pad = &sim.nav.pads[sim.nav.waypoints[w].padnum];
            (pad.pos, pad.flags)
        })
    };
    let c = &sim.chrs[i];
    let prop = c.prop_pos();
    let prevprop = c.prevpos + Vec3::Y * c.root_height();
    let curindex = c.act_gopos.curindex;
    let mut advance = false;
    if let Some((padpos, flags)) = pad_of(sim, curindex) {
        let arrivingxyz = pos_is_arriving_at_pos(prevprop, prop, padpos, 30.0);
        let gp = &mut sim.chrs[i].act_gopos;
        if flags.crouch {
            gp.crouch = true;
        } else if flags.duck {
            gp.duck = true;
        }
        if arrivingxyz {
            advance = true;
        }
    } else {
        // No more waypoints: arriving at the end point finishes the go-to.
        let end = sim.chrs[i].act_gopos.endpos;
        if pos_is_arriving_at_pos(prevprop, prop, end, 30.0) {
            sim.chrs[i].actiontype = Act::Stand;
            return;
        }
    }
    if advance {
        chr_gopos_advance_waypoint(sim, i);
    }

    let cyls = perims_except(sim, i);
    let radius = sim.chrs[i].radius;
    let can_move = |sim: &Sim, to: Vec3| chr_prop_can_move_to_pos_without_nav(&sim.chrs[i], &sim.level, to, radius * 1.2, &cyls);
    let point = |sim: &Sim, k: usize| pad_of(sim, k).map_or(sim.chrs[i].act_gopos.endpos, |p| p.0);

    // Every 10 ticks: skip two waypoints if neither is `PADFLAG_AIWALKDIRECT` and
    // the one after them (or the end) can be run to directly.
    let (age, init) = (sim.chrs[i].act_gopos.age, sim.chrs[i].act_gopos.init);
    if age % 10 == 5 || init {
        let cur = sim.chrs[i].act_gopos.curindex;
        if let (Some((_, f0)), Some((_, f1))) = (pad_of(sim, cur), pad_of(sim, cur + 1)) {
            if !f0.walkdirect && !f1.walkdirect && can_move(sim, point(sim, cur + 2)) {
                chr_gopos_advance_waypoint(sim, i);
                chr_gopos_advance_waypoint(sim, i);
            }
        }
    }
    // Every 10 ticks: skip the current waypoint if the next (or the end) can be
    // run to directly. A `PADFLAG_AIWALKDIRECT` waypoint is only skipped on the
    // first tick, and only if the chr is within 45 degrees of the line through it.
    if age % 10 == 0 || init {
        let cur = sim.chrs[i].act_gopos.curindex;
        if let Some((padpos, flags)) = pad_of(sim, cur) {
            let candosomething = init;
            if !flags.walkdirect || candosomething {
                let nextpos = point(sim, cur + 1);
                if flags.walkdirect && candosomething {
                    let p = sim.chrs[i].prop_pos();
                    let a = Vec2::new(p.x - padpos.x, p.z - padpos.z);
                    let b = Vec2::new(nextpos.x - padpos.x, nextpos.z - padpos.z);
                    let sp156 = (a.length_squared() * b.length_squared()).sqrt();
                    if sp156 > 0.0 {
                        let sp160 = (a.dot(b) / sp156).clamp(-1.0, 1.0).acos();
                        if (sp160 < baddtor(45.0) || sp160 > baddtor(315.0)) && can_move(sim, nextpos) {
                            chr_gopos_advance_waypoint(sim, i);
                        }
                    }
                } else if can_move(sim, nextpos) {
                    chr_gopos_advance_waypoint(sim, i);
                }
            }
        }
        sim.chrs[i].act_gopos.init = false;
    }

    let cur = sim.chrs[i].act_gopos.curindex;
    let target = point(sim, cur);
    let c = &mut sim.chrs[i];
    let d = Vec2::new(target.x - c.pos.x, target.z - c.pos.z);
    if d.length_squared() > 1e-6 {
        let mut a = d.x.atan2(d.y);
        if a < 0.0 {
            a += turn();
        }
        c.aibot.roty = a;
    }
}

// ─── Death ───────────────────────────────────────────────────────────────────

/// `chr_die` (`chraction.c:5102`), bot path.
pub fn chr_die(sim: &mut Sim, victim: usize, attacker: Option<usize>) {
    if sim.chrs[victim].actiontype == Act::Die {
        return;
    }
    sim.chrs[victim].actiontype = Act::Die;
    sim.chrs[victim].deaths += 1;
    match attacker {
        Some(a) if a != victim => sim.chrs[a].kills += 1,
        _ => sim.chrs[victim].suicides += 1,
    }
    sim.log(format!(
        "{} killed {}",
        attacker.map_or("?".to_string(), |a| sim.chrs[a].name.clone()),
        sim.chrs[victim].name
    ));
}

/// `chr_tick_die`: when the death animation reaches its end frame, `chr_begin_dead`.
fn chr_tick_die(sim: &mut Sim, i: usize) {
    let c = &mut sim.chrs[i];
    if c.model.animnum.is_some() && super::anims::DEATH_ANIMS.contains(&c.model.animnum.unwrap()) {
        if c.model.frame >= c.model.end_frame() {
            c.actiontype = Act::Dead;
            c.fadetimer60 = 0;
        }
    }
}

/// `chr_tick_dead` (`chraction.c:8229`): 90-tick fade, then respawn.
fn chr_tick_dead(sim: &mut Sim, i: usize) {
    let g = sim.g;
    let c = &mut sim.chrs[i];
    c.fadetimer60 += g.lvupdate60;
    if c.fadetimer60 >= 90 {
        c.fadealpha = 0.0;
        bot::bot_spawn(sim, i);
    } else {
        c.fadealpha = (90 - c.fadetimer60) as f32 * 255.0 / 90.0;
    }
}

// ─── Angles, sight ───────────────────────────────────────────────────────────

/// `chr_get_angle_to_pos` (`chraction.c:13787`): bearing to `pos` relative to
/// `theta`, in `[0, BADDTOR(360))`.
pub fn chr_get_angle_to_pos(c: &Chr, pos: Vec3) -> f32 {
    let mut a = (pos.x - c.pos.x).atan2(pos.z - c.pos.z) - c.theta();
    if a < 0.0 {
        a += turn();
    }
    a
}

/// `chr_is_target_in_fov(chr, degrees256, false)` (`chraction.c:13979`).
pub fn chr_is_target_in_fov(c: &Chr, target_pos: Vec3, degrees256: u8) -> bool {
    let d256 = degrees256 as f32 * (360.0 * M_BADPI / 180.0 / 256.0);
    let angle = chr_get_angle_to_pos(c, target_pos);
    (angle < d256 && angle < dtor(180.0)) || (angle > baddtor(360.0) - d256 && angle > dtor(180.0))
}

/// `chr_has_los_to_chr` (`chraction.c:6513`): from 20 cm below the top of this
/// chr's cylinder (`chr->ground + height - 20`) to the target's prop position,
/// against `GEOFLAG_BLOCK_SIGHT`. Other chrs never block sight.
pub fn chr_has_los_to_chr(sim: &Sim, i: usize, target: usize) -> bool {
    let c = &sim.chrs[i];
    let eye = Vec3::new(c.pos.x, c.ground + c.height - 20.0, c.pos.z);
    sim.level.los(eye, sim.chrs[target].prop_pos())
}

// ─── Aim ─────────────────────────────────────────────────────────────────────

/// `chr_calculate_aimend` (`chraction.c:9071`), aibot path: `holdturn` is false for
/// bots, so there is **no horizontal correction** (`aimendsideback = 0`) — the
/// barrel points where the body faces, zeroing error included. Only the vertical
/// aim is computed, from the chr's root to the target's.
pub fn chr_calculate_aimend(
    c: &mut Chr,
    target_pos: Vec3,
    animcfg: Option<&'static AttackAnimConfig>,
    hasleftgun: bool,
    hasrightgun: bool,
) {
    let from = c.prop_pos();
    let rel = target_pos - from;
    let mut shootroty = rel.y.atan2((rel.x * rel.x + rel.z * rel.z).sqrt());
    if shootroty >= dtor(180.0) {
        shootroty -= turn();
    }
    chr_calculate_aimend_vertical(c, animcfg, hasleftgun, hasrightgun, shootroty);
    c.aimendsideback = 0.0;
    c.aimendcount = 10;
}

/// `chr_calculate_aimend_vertical` (`chraction.c:9345`).
fn chr_calculate_aimend_vertical(
    c: &mut Chr,
    animcfg: Option<&'static AttackAnimConfig>,
    hasleftgun: bool,
    hasrightgun: bool,
    shootroty: f32,
) {
    let mut freearmangle = 0.0;
    let mut backangle = 0.0;
    let mut gunarmangle = shootroty;
    if let Some(cfg) = animcfg {
        if shootroty > cfg.maxup_rad() {
            backangle = shootroty - cfg.maxup_rad();
            gunarmangle = cfg.maxup_rad();
        } else if shootroty < cfg.maxdown_rad() {
            backangle = shootroty - cfg.maxdown_rad();
            gunarmangle = cfg.maxdown_rad();
        }
        freearmangle = if gunarmangle > 0.0 { cfg.freearmfracup * gunarmangle } else { cfg.freearmfracdown * gunarmangle };
    }
    if hasrightgun {
        c.aimendrshoulder = gunarmangle;
        c.aimendlshoulder = if hasleftgun { gunarmangle } else { freearmangle };
    } else {
        c.aimendrshoulder = freearmangle;
        c.aimendlshoulder = gunarmangle;
    }
    c.aimendback = backangle;
}

/// `chr_reset_aimend` (`chraction.c:9383`).
pub fn chr_reset_aimend(c: &mut Chr) {
    c.aimendcount = 10;
    c.aimendrshoulder = 0.0;
    c.aimendlshoulder = 0.0;
    c.aimendback = 0.0;
    c.aimendsideback = 0.0;
}

/// `chr_tween_aim` (`chr.c:1509`).
pub fn chr_tween_aim(c: &mut Chr, lvupdate60f: f32, lvupdate60: i32) {
    if c.aimendcount >= 2 {
        let mult = (lvupdate60f / c.aimendcount as f32).min(1.0);
        c.aimuplshoulder += (c.aimendlshoulder - c.aimuplshoulder) * mult;
        c.aimuprshoulder += (c.aimendrshoulder - c.aimuprshoulder) * mult;
        c.aimupback += (c.aimendback - c.aimupback) * mult;
        c.aimsideback += (c.aimendsideback - c.aimsideback) * mult;
        c.aimendcount -= lvupdate60;
    } else {
        c.aimuplshoulder = c.aimendlshoulder;
        c.aimuprshoulder = c.aimendrshoulder;
        c.aimupback = c.aimendback;
        c.aimsideback = c.aimendsideback;
    }
}

/// `chr_get_aimx_angle` — for a bot, `theta + aimsideback` (the bot branch adds
/// nothing).
pub fn chr_get_aimx_angle(c: &Chr) -> f32 {
    let mut a = c.theta() + c.aimsideback;
    if a >= turn() {
        a -= turn();
    } else if a < 0.0 {
        a += turn();
    }
    a
}

/// `chr_get_aimy_angle`.
pub fn chr_get_aimy_angle(c: &Chr) -> f32 {
    let mut s = c.aimuprshoulder + c.aimupback;
    if s < 0.0 {
        s += turn();
    }
    s
}

// ─── Shooting ────────────────────────────────────────────────────────────────

/// `chr_set_hand_firing` (`chraction.c:10107`).
pub fn chr_set_hand_firing(c: &mut Chr, hand: usize, firing: bool) {
    c.hand_firing[hand] = firing;
    if !firing {
        c.gunfire_visible[hand] = false;
    }
}

/// `chr_tick_shots` (`chraction.c:10550`).
fn chr_tick_shots(sim: &mut Sim, i: usize) {
    for hand in [HAND_RIGHT, HAND_LEFT] {
        if sim.chrs[i].hand_firing[hand] {
            chr_shoot(sim, i, hand);
            sim.chrs[i].hand_firing[hand] = false;
        }
    }
}

/// Where the shot starts: the rendered muzzle if we have one (`chr_get_gun_pos`),
/// else PD's off-screen estimate — 30 above the prop position, 10 to the side.
pub fn chr_gun_pos(c: &Chr, hand: usize) -> Vec3 {
    if let Some(p) = c.gunpos_rendered[hand] {
        return p;
    }
    let roty = chr_get_aimx_angle(c);
    let pp = c.prop_pos();
    let mut g = Vec3::new(pp.x, pp.y + 30.0, pp.z);
    if hand == HAND_LEFT {
        g.x += roty.cos() * 10.0;
        g.z += -roty.sin() * 10.0;
    } else {
        g.x += -roty.cos() * 10.0;
        g.z += roty.sin() * 10.0;
    }
    g
}

/// The un-spread shot direction (`chr_shoot`, `chraction.c:10096`).
pub fn chr_shot_dir(c: &Chr) -> Vec3 {
    let roty = chr_get_aimx_angle(c);
    let rotx = chr_get_aimy_angle(c);
    Vec3::new(rotx.cos() * roty.sin(), rotx.sin(), rotx.cos() * roty.cos())
}

/// `bgun_calculate_bot_shot_spread` (`bondgun.c:5142`): squatting halves the
/// spread, dual wielding multiplies it by 1.5.
pub fn bgun_calculate_bot_shot_spread(sim: &mut Sim, dir: Vec3, spread: f32, squat: bool, dual: bool) -> Vec3 {
    let mut spread = spread;
    if squat {
        spread *= 0.5;
    }
    if dual {
        spread *= 1.5;
    }
    let radius = 120.0 * spread / FOVY;
    let x = (sim.rng.randomfrac() - 0.5) * sim.rng.randomfrac() * radius;
    let y = (sim.rng.randomfrac() - 0.5) * sim.rng.randomfrac() * radius;
    let v = Vec3::new(C_SCALE * x, C_SCALE * y, -1.0).normalize();
    // mtx00016b58(look = dir, up = (0,-1,0)), then mtx4_rotate_vec.
    let look = -dir.normalize();
    let up = Vec3::new(0.0, -1.0, 0.0);
    let abc = Vec3::new(up.y * look.z - up.z * look.y, up.z * look.x - up.x * look.z, up.x * look.y - up.y * look.x)
        .normalize();
    let up2 = Vec3::new(
        look.y * abc.z - look.z * abc.y,
        look.z * abc.x - look.x * abc.z,
        look.x * abc.y - look.y * abc.x,
    )
    .normalize();
    abc * v.x + up2 * v.y + look * v.z
}

/// `chr_shoot` (`chraction.c:9916`), bot path, hitscan guns.
fn chr_shoot(sim: &mut Sim, i: usize, hand: usize) {
    let g = sim.g;
    let Some(wid) = sim.chrs[i].weapons_held[hand] else { return };
    let Some(w) = weapons::get(wid) else { return };
    let tickspershot = bot::weapon_get_num_ticks_per_shot(w);
    let mut shotdue = false;
    {
        let c = &mut sim.chrs[i];
        if tickspershot <= 0 {
            shotdue = true;
        } else {
            c.firecount[hand] += g.lvupdate60;
            if c.firecount[hand] >= tickspershot {
                c.firecount[hand] = 0;
                c.unk32c_12 ^= 1 << hand;
                shotdue = true;
            }
        }
    }
    let mut firingthisframe = false;
    if shotdue {
        firingthisframe = true;
        let c = &sim.chrs[i];
        let gunpos = chr_gun_pos(c, hand);
        // Don't fire a gun that's been pushed through a wall or into another chr.
        if !sim.level.los(c.prop_pos(), gunpos)
            || sim.chrs.iter().enumerate().any(|(j, o)| {
                j != i && o.actiontype != Act::Dead && (Vec2::new(gunpos.x, gunpos.z) - o.pos2()).length() < o.radius
            })
        {
            firingthisframe = false;
        }
        if firingthisframe {
            let dual = c.weapons_held[0].is_some() && c.weapons_held[1].is_some();
            let dir0 = chr_shot_dir(c);
            let squat = thirdperson::bot_guess_crouch_pos(c.height) == thirdperson::CrouchPos::Squat;
            let dir = bgun_calculate_bot_shot_spread(sim, dir0, w.spread, squat, dual);
            // The first thing the ray meets takes the hit — including a bystander.
            let wall = sim.level.raycast_shoot(gunpos, dir, 65536.0);
            let mut best: Option<(f32, usize)> = None;
            for (j, o) in sim.chrs.iter().enumerate() {
                if j == i || o.actiontype == Act::Dead {
                    continue;
                }
                if let Some(t) = ray_vs_cylinder(gunpos, dir, o.pos, o.radius, o.height) {
                    if best.map_or(true, |(bt, _)| t < bt) {
                        best = Some((t, j));
                    }
                }
            }
            let wall_t = wall.map_or(65536.0, |h| h.dist);
            let (end, hit_chr) = match best {
                Some((t, j)) if t < wall_t => (gunpos + dir * t, Some(j)),
                _ => (gunpos + dir * wall_t, None),
            };
            sim.shots.push(Shot { from: gunpos, to: end, hit_chr, age: 0, shooter: i });
            if let Some(j) = hit_chr {
                chr_damage(sim, j, w.damage, dir, Some(i), false);
            }
        }
    }
    let c = &mut sim.chrs[i];
    if firingthisframe && c.aibot.loadedammo[hand] > 0 {
        c.aibot.loadedammo[hand] -= 1;
    }
    c.gunfire_visible[hand] = firingthisframe;
}

/// Ray vs a vertical cylinder standing on `base` (PD's `useperimshoot`: shots test
/// the chr's perimeter cylinder, not its body parts). Returns the distance.
fn ray_vs_cylinder(o: Vec3, d: Vec3, base: Vec3, r: f32, h: f32) -> Option<f32> {
    let oc = Vec2::new(o.x - base.x, o.z - base.z);
    let dd = Vec2::new(d.x, d.z);
    let a = dd.length_squared();
    if a < 1e-9 {
        return None;
    }
    let b = oc.dot(dd);
    let cc = oc.length_squared() - r * r;
    let disc = b * b - a * cc;
    if disc < 0.0 {
        return None;
    }
    let t = (-b - disc.sqrt()) / a;
    if t < 0.0 {
        return None;
    }
    let y = o.y + d.y * t;
    (y >= base.y && y <= base.y + h).then_some(t)
}

/// `chr_punch_inflict_damage` (`chraction.c:7732`): lands only inside a tight
/// 28° cone, closer than `range`, with a clear line.
pub fn chr_punch_inflict_damage(sim: &mut Sim, i: usize, damage: f32, range: f32) {
    let c = &sim.chrs[i];
    let Some(t) = c.target else { return };
    let tp = sim.chrs[t].prop_pos();
    let cp = c.prop_pos();
    if chr_is_target_in_fov(c, tp, 20) && cp.distance(tp) < range && sim.level.los(cp, tp) {
        let v = Vec3::new(tp.x - cp.x, 0.0, tp.z - cp.z).normalize_or_zero();
        // Unarmed punch: funcdef_melee damage 0.5 (`invitems.c:273`).
        chr_damage(sim, t, 0.5 * damage, v, Some(i), true);
    }
}

// ─── Damage ──────────────────────────────────────────────────────────────────

/// `chr_damage` (`chraction.c:4144`), the path a bot victim takes in normal
/// multiplayer: no stun, no hit animation — a shove, a flinch, and death at
/// `maxdamage`. Bullets arrive as `HITPART_GENERAL` (×0.5, then the torso ×2).
pub fn chr_damage(sim: &mut Sim, victim: usize, damage: f32, vector: Vec3, attacker: Option<usize>, blunt: bool) {
    let mut damage = damage;
    if sim.chrs[victim].is_dead() {
        return;
    }
    if blunt {
        // FUNCFLAG_BLUNTIMPACT: punches are weaker from the front.
        let c = &sim.chrs[victim];
        let angle = chr_get_angle_to_pos(c, c.prop_pos() - vector);
        if angle < baddtor(60.0) || angle > baddtor(300.0) {
            damage *= 0.4;
        } else if angle < baddtor(120.0) || angle > 4.188_123_703_002_9 {
            damage *= 0.7;
        }
    }
    // HITPART_GENERAL → torso at half damage, then torso doubles it.
    damage *= 0.5;
    damage += damage;

    // `chr_flinch_body` only calls random() when a flinch actually starts.
    let flinch_roll = if sim.chrs[victim].flinchcnt < 0 { sim.rng.random() } else { 0 };
    let c = &mut sim.chrs[victim];
    if c.damage < c.maxdamage {
        c.aibot.shotspeed.x += vector.x * 0.75;
        c.aibot.shotspeed.z += vector.z * 0.75;
        if damage > 0.0 {
            c.damage += damage;
            chr_flinch_body(c, flinch_roll);
            if c.damage >= c.maxdamage {
                chr_die(sim, victim, attacker);
            }
        }
    }
}

/// `chr_flinch_body` (`chr.c:1532`).
pub fn chr_flinch_body(c: &mut Chr, r: u32) {
    if c.actiontype != Act::Dead && c.flinchcnt < 0 {
        c.flinchcnt = 1;
        c.flinchtype = (r & 7) as u8;
    }
}

/// `chr_get_flinch_amount` (`chr.c:1566`), body branch.
pub fn chr_get_flinch_amount(c: &Chr) -> f32 {
    let v = c.flinchcnt as f32;
    if v < 10.0 {
        (v * baddtor(90.0) / 10.0).sin()
    } else {
        1.0 - ((v - 10.0) * 0.078_527_316_451_073).sin()
    }
}
