//! Headless runs of the menus with a scripted controller, saving frames as
//! PNGs — the way to look at the menus without opening (or driving) a window.
//!
//! `pd_combat_sim_snapshot <outdir> [--fresh] [--combat] <script...>` where the
//! script is whitespace-separated steps:
//!
//! * `w<N>` — run N frames with nothing held (`w30`)
//! * `a` `b` `z` `start` `up` `down` `left` `right` `l` `r` `cu` `cd` `cl` `cr`
//!   — tap that button (held one frame, released the next, then 6 frames)
//! * `p2start` `p3start` `p4start` — START on controller 2-4
//! * `sx<V>` / `sy<V>` — hold the stick at V (−80..80) until changed
//! * `shot:<name>` — write `<outdir>/<name>.png` (the 320×220 frame, ×3)
//! * `bs` — the keyboard item's delete

use std::path::Path;

use super::mp::Profile;
use super::types::*;
use super::Pd;

pub fn write_png(pd: &Pd, path: &Path, scale: u32) -> Result<(), String> {
    let (w, h) = (pd.gfx.w as u32, pd.gfx.h as u32);
    let rgba = pd.gfx.rgba8(true);
    let mut img = image::RgbaImage::new(w * scale, h * scale);
    for y in 0..h * scale {
        for x in 0..w * scale {
            let i = (((y / scale) * w + x / scale) * 4) as usize;
            img.put_pixel(x, y, image::Rgba([rgba[i], rgba[i + 1], rgba[i + 2], 255]));
        }
    }
    img.save(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn tap(pd: &mut Pd, pad: usize, bit: u16) {
    pd.joy[pad].buttons |= bit;
    pd.frame(1);
    pd.joy[pad].buttons &= !bit;
    for _ in 0..6 {
        pd.frame(1);
    }
}

/// Run `script` and return the (name, png path) of every shot.
pub fn run_script(pd: &mut Pd, outdir: &Path, script: &[String]) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(outdir).map_err(|e| e.to_string())?;
    let mut shots = Vec::new();
    for step in script {
        let s = step.as_str();
        let bit = match s {
            "a" => Some((0, A_BUTTON)),
            "b" => Some((0, B_BUTTON)),
            "z" => Some((0, Z_TRIG)),
            "start" => Some((0, START_BUTTON)),
            "up" => Some((0, U_JPAD)),
            "down" => Some((0, D_JPAD)),
            "left" => Some((0, L_JPAD)),
            "right" => Some((0, R_JPAD)),
            "l" => Some((0, L_TRIG)),
            "r" => Some((0, R_TRIG)),
            "cu" => Some((0, U_CBUTTONS)),
            "cd" => Some((0, D_CBUTTONS)),
            "cl" => Some((0, L_CBUTTONS)),
            "cr" => Some((0, R_CBUTTONS)),
            "p2start" => Some((1, START_BUTTON)),
            "p3start" => Some((2, START_BUTTON)),
            "p4start" => Some((3, START_BUTTON)),
            _ => None,
        };
        if let Some((p, b)) = bit {
            if p > 0 {
                pd.connected_pads |= 1 << p;
            }
            tap(pd, p, b);
        } else if s == "bs" {
            pd.joy[0].back2 = true;
            pd.frame(1);
        } else if let Some(n) = s.strip_prefix('w') {
            let n: u32 = n.parse().map_err(|_| format!("bad wait {s}"))?;
            for _ in 0..n {
                pd.frame(1);
            }
        } else if let Some(v) = s.strip_prefix("sx") {
            pd.joy[0].stick_x = v.parse().map_err(|_| format!("bad stick {s}"))?;
        } else if let Some(v) = s.strip_prefix("sy") {
            pd.joy[0].stick_y = v.parse().map_err(|_| format!("bad stick {s}"))?;
        } else if let Some(name) = s.strip_prefix("shot:") {
            let path = outdir.join(format!("{name}.png"));
            write_png(pd, &path, 3)?;
            shots.push(path.display().to_string());
        } else {
            return Err(format!("unknown step {s}"));
        }
    }
    Ok(shots)
}

pub fn main() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).try_init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(outdir) = args.first() else {
        eprintln!("usage: pd_combat_sim_snapshot <outdir> [--fresh] [--combat] <script...>");
        std::process::exit(2);
    };
    let profile = if args.iter().any(|a| a == "--fresh") { Profile::Fresh } else { Profile::Complete };
    let mut pd = Pd::new(profile).unwrap_or_else(|e| panic!("{e}"));
    if args.iter().any(|a| a == "--combat") {
        pd.open_combat_simulator();
    } else {
        pd.open_main_menu();
    }
    let script: Vec<String> = args[1..].iter().filter(|a| !a.starts_with("--")).cloned().collect();
    match run_script(&mut pd, Path::new(outdir), &script) {
        Ok(shots) => {
            for s in shots {
                println!("{s}");
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
