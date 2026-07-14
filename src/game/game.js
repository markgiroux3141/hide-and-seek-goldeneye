// game — orchestrates the BUILD -> HUNT loop for the vertical slice.
//
// BUILD phase = the editor as-is (fly camera, live CSG). Pressing the start key
// FREEZES the level: we bake a NavWorld from the current geometry, switch the
// camera to an FPS capsule controller, and spawn hunters. The level geometry is
// immutable during the hunt (gadgets come later; no CSG edits).

import { bakeNavWorld } from './navGrid.js';
import { buildDoors } from './door.js';
import { Player } from './player.js';
import { Enemy } from './enemy.js';

const g = {
    phase: 'build',          // 'build' | 'hunt' | 'won' | 'lost'
    nav: null,
    player: null,
    enemies: [],
    scene: null,
    camera: null,
    doors: [],
    huntTime: 0,
    surviveGoal: 30,         // seconds to survive one wave (slice win condition)
    hud: null,
    spawn: null,             // player spawn cell (for placing hunters far away)
    waveSize: 16,            // hunters spawned at hunt start (scale test)
    invuln: false,           // toggle for perf observation (K)
    // Perf instrumentation (scale test)
    aiMs: 0,                 // rolling avg ms/frame spent in enemy AI+pathing
    bakeMs: 0,               // nav bake time at transition
    cells: 0,                // nav grid cell count
    memKB: 0,                // nav grid memory
};

export function initGame(camera, scene) {
    g.camera = camera;
    g.scene = scene;
    g.hud = makeHud();
    updateHud();
}

export function isHuntActive() { return g.phase === 'hunt'; }
export function getPhase() { return g.phase; }

// Build -> Hunt transition: freeze + bake + spawn.
export function startHunt() {
    if (g.phase === 'hunt') return;

    const t0 = performance.now();
    const nav = bakeNavWorld();
    g.bakeMs = performance.now() - t0;
    if (!nav) { flash('Nothing built yet — carve a room first.'); return; }
    g.nav = nav;
    g.cells = nav.nx * nav.ny * nav.nz;
    g.memKB = Math.round(nav.solid.length / 1024);
    console.log(`[nav bake] ${g.bakeMs.toFixed(1)}ms  grid ${nav.nx}x${nav.ny}x${nav.nz} = ${g.cells.toLocaleString()} cells (${g.memKB} KB)`);

    // Spawn the player at the standable cell nearest the current camera.
    const cam = g.camera.position;
    const spawn = nav.nearestStandable(cam.x, cam.y, cam.z, 48);
    if (!spawn) { flash('No standable floor found to spawn on.'); return; }
    g.spawn = spawn;
    g.player = new Player(g.camera, nav);
    g.player.spawnAt(nav.cellFloorMeters(spawn.ix, spawn.iy, spawn.iz));

    // Breakable doors from the door-tool openings (dynamic overlay on the grid).
    g.doors = buildDoors(g.scene, nav);

    g.enemies = [];
    g.aiMs = 0;
    spawnHunters(g.waveSize);

    g.huntTime = 0;
    g.invuln = false;
    g.phase = 'hunt';
    updateHud();
    flash(`HUNT! ${g.enemies.length} hunters. E=+4  K=godmode  WASD/mouse to move.`);
}

// Spawn n hunters on standable cells biased toward the far side of the level.
function spawnHunters(n) {
    if (!g.nav || !g.spawn) return;
    const cells = (g.nav.__standable ||= g.nav.allStandable());
    if (!cells.length) return;
    const sp = g.spawn;
    const scored = cells
        .map(c => ({ c, d: (c.ix - sp.ix) ** 2 + (c.iy - sp.iy) ** 2 + (c.iz - sp.iz) ** 2 }))
        .sort((a, b) => b.d - a.d);
    const pool = scored.slice(0, Math.max(1, Math.floor(scored.length * 0.6)));
    for (let i = 0; i < n; i++) {
        const pick = pool[(Math.random() * pool.length) | 0].c;
        const e = new Enemy(g.scene, g.nav, g.nav.cellFloorMeters(pick.ix, pick.iy, pick.iz));
        e.onBreach = onDoorBreach;
        g.enemies.push(e);
    }
    updateHud();
}

// A hunter broke a door: drop the panel and surface the noise to the player
// (noise = information — they hear where the breach happened).
function onDoorBreach(door) {
    if (door.mesh) { g.scene.remove(door.mesh); door.mesh.geometry.dispose(); door.mesh = null; }
    flash('DOOR BREACHED!');
}

// Hunt-phase hotkeys (routed from main while hunting): pile on hunters / godmode.
export function handleHuntKey(e) {
    if (g.phase !== 'hunt') return;
    if (e.code === 'KeyE') { spawnHunters(4); flash(`+4 hunters (${g.enemies.length} total)`); }
    else if (e.code === 'KeyK') { g.invuln = !g.invuln; flash('God mode: ' + (g.invuln ? 'ON' : 'OFF')); }
}

// Called every frame from the render loop while hunting.
export function updateGame(dt) {
    if (g.phase !== 'hunt') return;
    g.player.update(dt);
    g.huntTime += dt;

    // Time the enemy AI + pathing block (the scale-test measurement).
    const t0 = performance.now();
    let caught = false;
    for (const e of g.enemies) {
        if (e.update(dt, g.player.pos)) caught = true;
    }
    g.aiMs = g.aiMs * 0.9 + (performance.now() - t0) * 0.1;

    if (caught && !g.invuln) { endHunt('lost'); return; }

    const spotted = g.enemies.some(e => e.state === 'chase');
    const searching = g.enemies.some(e => e.state === 'search');
    setAlert(spotted ? 'spotted' : searching ? 'search' : 'hidden');

    if (g.huntTime >= g.surviveGoal && !g.invuln) { endHunt('won'); return; }
    updateHud();
}

function endHunt(result) {
    g.phase = result; // 'won' | 'lost'
    for (const e of g.enemies) e.dispose(g.scene);
    g.enemies = [];
    for (const d of g.doors) if (d.mesh) { g.scene.remove(d.mesh); d.mesh.geometry.dispose(); d.mesh = null; }
    g.doors = [];
    setAlert('hidden');
    updateHud();
    flash(result === 'won' ? 'YOU SURVIVED THE WAVE!' : 'CAUGHT. You were found.');
}

// ─── HUD overlay ─────────────────────────────────────────────────────
function makeHud() {
    const el = document.createElement('div');
    el.id = 'game-hud';
    el.style.cssText = [
        'position:fixed', 'top:8px', 'left:50%', 'transform:translateX(-50%)',
        'font:600 14px/1.4 monospace', 'color:#fff', 'text-align:center',
        'padding:6px 12px', 'background:rgba(0,0,0,0.45)', 'border-radius:6px',
        'pointer-events:none', 'z-index:1000', 'white-space:pre',
    ].join(';');
    document.body.appendChild(el);

    const toast = document.createElement('div');
    toast.id = 'game-toast';
    toast.style.cssText = [
        'position:fixed', 'top:50%', 'left:50%', 'transform:translate(-50%,-50%)',
        'font:700 22px/1.4 monospace', 'color:#ffd', 'text-align:center',
        'padding:10px 18px', 'background:rgba(0,0,0,0.6)', 'border-radius:8px',
        'pointer-events:none', 'z-index:1001', 'opacity:0', 'transition:opacity .3s',
    ].join(';');
    document.body.appendChild(toast);

    // Red screen-edge vignette that fades in when a hunter has eyes on you.
    const vignette = document.createElement('div');
    vignette.id = 'game-vignette';
    vignette.style.cssText = [
        'position:fixed', 'inset:0', 'pointer-events:none', 'z-index:999',
        'opacity:0', 'transition:opacity .15s',
        'box-shadow:inset 0 0 140px 40px rgba(200,20,20,0.85)',
    ].join(';');
    document.body.appendChild(vignette);
    return { el, toast, vignette, alert: 'hidden' };
}

// alert: 'hidden' | 'search' | 'spotted'
function setAlert(level) {
    if (!g.hud) return;
    g.hud.vignette.style.opacity = level === 'spotted' ? '1' : level === 'search' ? '0.35' : '0';
    g.hud.alert = level;
}

let _toastTimer = null;
function flash(msg) {
    if (!g.hud) return;
    g.hud.toast.textContent = msg;
    g.hud.toast.style.opacity = '1';
    clearTimeout(_toastTimer);
    _toastTimer = setTimeout(() => { g.hud.toast.style.opacity = '0'; }, 2200);
}

function updateHud() {
    if (!g.hud) return;
    if (g.phase === 'build') {
        g.hud.el.textContent = 'BUILD PHASE — press G to start the hunt';
    } else if (g.phase === 'hunt') {
        const left = Math.max(0, g.surviveGoal - g.huntTime).toFixed(1);
        const status = g.hud.alert === 'spotted' ? 'SPOTTED!' : g.hud.alert === 'search' ? 'hunted…' : 'hidden';
        const timer = g.invuln ? 'GODMODE' : `survive: ${left}s`;
        const intact = g.doors.filter(d => !d.broken).length;
        const doorInfo = g.doors.length ? `   doors: ${intact}/${g.doors.length}` : '';
        g.hud.el.textContent =
            `HUNT — ${timer}   hunters: ${g.enemies.length}   [${status}]${doorInfo}\n` +
            `AI: ${g.aiMs.toFixed(2)} ms/frame   nav: ${g.cells.toLocaleString()} cells (${g.memKB} KB, baked ${g.bakeMs.toFixed(0)} ms)`;
    } else if (g.phase === 'won') {
        g.hud.el.textContent = 'SURVIVED — reload to build again';
    } else if (g.phase === 'lost') {
        g.hud.el.textContent = 'CAUGHT — reload to build again';
    }
}
