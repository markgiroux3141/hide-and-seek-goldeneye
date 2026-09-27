//! The exported Perfect Dark data (`tools/pd-assets/pd_fpgun.py all`), as typed
//! Rust. Nothing here interprets anything — it is `weapons.json` and the
//! per-model JSON, with the few fields whose C shape is awkward (function unions,
//! symbol references) resolved into something indexable.
//!
//! Units are PD's: model units are what the model file stores (millimetres for a
//! gun: `bgun0f0a5550` scales the root by 0.1 into world centimetres), angles are
//! radians in PD's `BADDTOR` unit, time is 60 Hz ticks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

/// `native/assets/weapons/pd_fp/`.
pub fn assets_dir() -> PathBuf {
    PathBuf::from(format!("{}/../../assets/weapons/pd_fp", env!("CARGO_MANIFEST_DIR")))
}

// ─── weapons.json ────────────────────────────────────────────────────────────

#[derive(Deserialize, Debug, Clone)]
pub struct WeaponsFile {
    pub weapons: Vec<RawWeapon>,
    pub scripts: HashMap<String, RawScript>,
    pub aimsettings: HashMap<String, AimSettings>,
    pub recoilsettings: HashMap<String, RecoilSettings>,
    pub noisesettings: HashMap<String, NoiseSettings>,
    pub gunviscmds: HashMap<String, Vec<RawGunVis>>,
    pub anims: HashMap<String, AnimMeta>,
    pub models: HashMap<String, ModelMeta>,
    pub head_anims: HashMap<String, u16>,
    /// `enum sfxnum` + `enum sfxmap` names → ids.
    #[serde(default)]
    pub sfx: HashMap<String, i64>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RawWeapon {
    pub weapon: String,
    pub weaponnum: i32,
    pub symbol: String,
    pub source: String,
    pub name_text: Option<String>,
    pub short_text: Option<String>,
    pub muzzlez: f32,
    pub posx: f32,
    pub posy: f32,
    pub posz: f32,
    pub sway: f32,
    pub weapon_flags: u32,
    #[serde(default)]
    pub assets: Option<RawAssets>,
    #[serde(default)]
    pub mp: Option<RawMp>,
    #[serde(default)]
    pub functions: Vec<Option<Value>>,
    #[serde(default)]
    pub ammo: Vec<Option<RawAmmo>>,
    pub equip_animation: Option<String>,
    pub unequip_animation: Option<String>,
    pub pritosec_animation: Option<String>,
    pub sectopri_animation: Option<String>,
    pub aimsettings: Option<String>,
    pub gunviscmds_symbol: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RawAssets {
    pub fp_model: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RawMp {
    pub pri_ammo_type: i32,
    pub pri_ammo_qty: i32,
    pub sec_ammo_type: i32,
    pub sec_ammo_qty: i32,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RawAmmo {
    #[serde(rename = "type")]
    pub ammotype: i32,
    pub casingeject: i32,
    pub clipsize: i32,
    pub reload_animation: Option<String>,
    pub flags: i64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RawScript {
    pub line: u32,
    pub cmds: Vec<Value>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct AimSettings {
    pub zoomfov: f32,
    pub guntransup: f32,
    pub guntransdown: f32,
    pub guntransside: f32,
    pub aimdamppal: f32,
    pub aimdamp: f32,
    pub tracktype: Value,
    pub flags: Value,
}

#[derive(Deserialize, Debug, Clone, Copy, Default)]
pub struct RecoilSettings {
    pub xrange: f32,
    pub yrange: f32,
    pub zrange: f32,
}

#[derive(Deserialize, Debug, Clone, Copy, Default)]
pub struct NoiseSettings {
    pub minradius: f32,
    pub maxradius: f32,
    pub incradius: f32,
    pub decbasespeed: f32,
    pub decremspeed: f32,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RawGunVis {
    pub op: String,
    pub args: Vec<Value>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct AnimMeta {
    pub id: String,
    pub file: String,
    pub numframes: u32,
    pub bytesperframe: u32,
    pub headerlen: u32,
    pub framelen: u32,
    pub flags: u32,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ModelMeta {
    pub file: String,
}

// ─── per-model JSON ──────────────────────────────────────────────────────────

#[derive(Deserialize, Debug, Clone)]
pub struct ModelFile {
    pub name: String,
    pub nummatrices: usize,
    pub nodes: Vec<RawNode>,
    pub parts: HashMap<String, usize>,
    pub materials: Vec<Material>,
    pub batches: Vec<Batch>,
    pub textures: HashMap<String, TextureRef>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RawNode {
    #[serde(rename = "type")]
    pub kind: String,
    pub parent: i32,
    pub partnum: Option<i32>,
    pub pos: Option<[f32; 3]>,
    pub animpart: Option<u16>,
    pub mtx: Option<[i16; 3]>,
    pub flags: Option<u32>,
    pub rw: Option<u16>,
    pub target: Option<i32>,
    pub near: Option<f32>,
    pub far: Option<f32>,
    pub rendermode: Option<i32>,
    pub cull_exit: Option<String>,
    pub quads: Option<Vec<[[i16; 3]; 4]>>,
    pub batches: Option<Vec<usize>>,
    /// BBOX nodes: `[xmin, xmax, ymin, ymax, zmin, zmax]` (`modelrodata_bbox`).
    pub bbox: Option<[f32; 6]>,
    /// CHRGUNFIRE nodes: flash size, texture number and its size.
    pub dim: Option<[f32; 3]>,
    pub texture: Option<u32>,
    pub texture_size: Option<[f32; 2]>,
}

/// One interpreted N64 draw state. See `pd_fpgun.py`'s `Interp.material_key`.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Material {
    pub two_cycle: bool,
    /// fast3d mux order: `[a0,b0,c0,d0, Aa0,Ab0,Ac0,Ad0, a1,b1,c1,d1, Aa1,Ab1,Ac1,Ad1]`.
    pub combine: [u8; 16],
    /// "inherit" | "none" | "back" | "front" | "both".
    pub cull: String,
    pub lighting: bool,
    pub texgen: bool,
    pub texgen_linear: bool,
    pub texture: Option<MatTexture>,
    pub prim: [u8; 4],
    pub env: Option<[u8; 4]>,
    pub fog: Option<[u8; 4]>,
    /// "opaque" | "alpha".
    pub blend: String,
    pub ztest: bool,
    pub zwrite: bool,
    pub decal: bool,
    /// "none" | "edge" | "threshold".
    pub alpha_test: String,
    pub fog_tint: bool,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct MatTexture {
    pub id: u32,
    /// 0 wrap, 1 clamp, 2 mirror (PD's TXMODE_*).
    pub cms: u8,
    pub cmt: u8,
    pub shifts: u8,
    pub shiftt: u8,
    pub uls: f32,
    pub ult: f32,
    pub mipmap: bool,
    pub linear: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Batch {
    pub node: usize,
    pub material: usize,
    /// `[x, y, z, mtx, u, v, c0, c1, c2, c3, flags]` — see the model's `vertex_layout`.
    pub verts: Vec<[f32; 11]>,
    pub indices: Vec<u32>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct TextureRef {
    pub file: String,
    pub w: u32,
    pub h: u32,
}

pub fn load_weapons(dir: &Path) -> Result<WeaponsFile, String> {
    let path = dir.join("weapons.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn load_model(dir: &Path, stem: &str) -> Result<ModelFile, String> {
    let path = dir.join("models").join(format!("{stem}.json"));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// A JSON number or `null`/missing as `f32`.
pub fn num(v: &Value, key: &str) -> f32 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0) as f32
}

pub fn int(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

pub fn string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}
