//! Render PD sounds through the range's TV-speaker chain offline (no audio device),
//! for spectrum checks — see `game::pd_guns::tvaudio`.
//!
//! `cargo run --release --bin pd_tv_audio -- <outdir> [--rate R] [--gain G] [sfx names...]`
//!
//! Each `native/assets/audio/pd/sfx/<name>.wav` is upsampled to 48 kHz (windowed
//! sinc, as kira would play it at playback rate `R`) and written once per
//! configuration: `<name>_<config>.wav`, 32-bit float stereo (so peaks over
//! 0 dBFS are kept for measuring). `G` scales the input (PD's per-sound volume).

use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use game::pd_guns::tvaudio::{AudioSettings, SpeakerPreset, TvChain};

const FS: u32 = 48_000;

/// The Falcon, shotgun, DY357, CMP150, rocket launch, Reaper, a reload, dry fire.
const DEFAULT: [&str; 8] = ["0066", "047d", "0074", "006e", "0001", "006d", "04fb", "0059"];

fn read_wav(path: &Path) -> Result<(u32, Vec<f64>), String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(format!("{}: not a WAV", path.display()));
    }
    let (mut rate, mut chans, mut bits, mut data) = (0u32, 0u16, 0u16, None);
    let mut i = 12;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32::from_le_bytes(b[i + 4..i + 8].try_into().unwrap()) as usize;
        let body = &b[i + 8..(i + 8 + len).min(b.len())];
        match id {
            b"fmt " => {
                chans = u16::from_le_bytes([body[2], body[3]]);
                rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                bits = u16::from_le_bytes([body[14], body[15]]);
            }
            b"data" => data = Some(body),
            _ => {}
        }
        i += 8 + len + (len & 1);
    }
    let data = data.ok_or("no data chunk")?;
    if bits != 16 || chans == 0 {
        return Err(format!("{}: {bits}-bit x{chans} unsupported", path.display()));
    }
    let n = chans as usize;
    let s = data
        .chunks_exact(2 * n)
        .map(|f| f.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f64 / 32768.0).sum::<f64>() / n as f64)
        .collect();
    Ok((rate, s))
}

fn write_wav(path: &Path, frames: &[(f32, f32)]) -> std::io::Result<()> {
    let mut b = Vec::with_capacity(44 + frames.len() * 8);
    let data_len = (frames.len() * 8) as u32;
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&FS.to_le_bytes());
    b.extend_from_slice(&(FS * 8).to_le_bytes());
    b.extend_from_slice(&8u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for &(l, r) in frames {
        b.extend_from_slice(&l.to_le_bytes());
        b.extend_from_slice(&r.to_le_bytes());
    }
    std::fs::write(path, b)
}

/// Blackman-windowed sinc resampler, cut at 0.95 of the lower Nyquist.
fn resample(x: &[f64], fin: f64, fout: f64) -> Vec<f64> {
    const HALF: i64 = 32;
    let ratio = fin / fout;
    let fc = 0.95 * 0.5 * fin.min(fout) / fin; // cycles per input sample
    let n = (x.len() as f64 / ratio).ceil() as usize;
    (0..n)
        .map(|j| {
            let t = j as f64 * ratio;
            let c = t.floor() as i64;
            let mut acc = 0.0;
            for k in c - HALF + 1..=c + HALF {
                if k < 0 || k as usize >= x.len() {
                    continue;
                }
                let d = t - k as f64;
                let w = 0.42 + 0.5 * (PI * d / HALF as f64).cos() + 0.08 * (2.0 * PI * d / HALF as f64).cos();
                let s = if d.abs() < 1e-12 { 2.0 * fc } else { (2.0 * PI * fc * d).sin() / (PI * d) };
                acc += x[k as usize] * s * w;
            }
            acc
        })
        .collect()
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: pd_tv_audio <outdir> [--rate R] [sfx names...]");
        std::process::exit(2);
    }
    let out = PathBuf::from(args.remove(0));
    let mut opt = |name: &str| {
        let i = args.iter().position(|a| a == name)?;
        let v = args.get(i + 1).and_then(|r| r.parse::<f64>().ok());
        args.drain(i..(i + 2).min(args.len()));
        v
    };
    let rate = opt("--rate").unwrap_or(1.0);
    let gain = opt("--gain").unwrap_or(1.0);
    let names: Vec<String> = if args.is_empty() { DEFAULT.iter().map(|s| s.to_string()).collect() } else { args };
    std::fs::create_dir_all(&out).expect("create outdir");
    let sfx = PathBuf::from(format!("{}/../../assets/audio/pd/sfx", env!("CARGO_MANIFEST_DIR")));

    let tv = |p: SpeakerPreset| {
        let mut s = AudioSettings { n64: true, tv: true, ..AudioSettings::default() };
        p.apply(&mut s);
        s
    };
    let configs: Vec<(&str, AudioSettings)> = vec![
        ("off", AudioSettings::default()),
        ("n64", AudioSettings { n64: true, ..AudioSettings::default() }),
        ("n64raw", AudioSettings { n64: true, dac_filter: false, ..AudioSettings::default() }),
        ("bigset", tv(SpeakerPreset::BigSet)),
        ("portable", tv(SpeakerPreset::Portable)),
        ("kitchen", tv(SpeakerPreset::Kitchen)),
    ];

    for name in &names {
        let (fin, x) = match read_wav(&sfx.join(format!("{name}.wav"))) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skip {name}: {e}");
                continue;
            }
        };
        let mut up: Vec<f64> = resample(&x, fin as f64 * rate, FS as f64).into_iter().map(|v| v * gain).collect();
        up.extend(std::iter::repeat_n(0.0, FS as usize / 4)); // let tails ring out
        for (cfg, s) in &configs {
            let mut chain = TvChain::new(FS, *s);
            let frames: Vec<(f32, f32)> = up
                .iter()
                .enumerate()
                .map(|(i, &v)| {
                    if i % 512 == 0 {
                        chain.flush();
                    }
                    chain.frame(v as f32, v as f32)
                })
                .collect();
            let peak = frames.iter().fold(0f32, |m, f| m.max(f.0.abs()).max(f.1.abs()));
            let path = out.join(format!("{name}_{cfg}.wav"));
            write_wav(&path, &frames).expect("write wav");
            println!("{name} ({fin} Hz x{rate}) {cfg:>8}: peak {:+.1} dBFS -> {}", 20.0 * peak.max(1e-9).log10(), path.display());
        }
    }
}
