//! Perfect Dark models for the menus: the `.pdm` files `pd_menu_models.py`
//! exports (chr bodies, heads, the hudpiece), PD's matrix walk over their node
//! tree, and a CPU rasteriser for the display lists they draw.
//!
//! **Matrix walk** — `model_set_matrices_with_anim` (model.c:1568): CHRINFO
//! roots (`model_update_chr_node_mtx`, model.c:726), POSITION nodes including
//! the elbow / knee helper matrices of `MODELNODETYPE_0100` / `_0200`
//! (`model_position_joint_using_vec_rot`, model.c:834), TOGGLE / DISTANCE /
//! HEADSPOT relations. The gun spike's walker (`pd_guns::model`) skips the
//! helper matrices, which every chr body uses, so this is a menu-local copy of
//! it; the animation data and the anim ticker are the gun spike's.
//!
//! **Heads** — `model_attach_head` (model.c:4275) makes the head's root a
//! child of the body's HEADSPOT. The head's display lists load matrix 0 from
//! the *body's* matrix segment (`SPSEGMENT_MODEL_MTX = model->matrices`,
//! model.c:3525), which is the headspot's parent joint, so a head needs no
//! matrices of its own.
//!
//! **Rasteriser** — what `pdgun.wgsl` does on the GPU, per pixel on the CPU:
//! RSP lighting (ambient + one directional light, the light direction taken
//! through the transposed modelview) and G_TEXTURE_GEN in the vertex stage,
//! then the RDP's two-cycle combiner evaluated literally from the 16 mux ids,
//! TRILERP mip levels from a per-triangle LOD, 3-point filtering, the
//! texture-edge alpha compare, and the XLU blender.
//!
//! **Substitutions:** coverage anti-aliasing is not emulated; the texture LOD
//! is computed once per triangle rather than per 2×2 pixel block; mip levels
//! below the base are box-filtered from it (the ROM stores its own).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use glam::{Mat3, Mat4, Quat, Vec3, Vec4};
use serde::Deserialize;

use crate::pd_guns::anim::{Anim, AnimCtx, ChrInfo};
use crate::pd_guns::animdata::{AnimBank, ANIMFLAG_ABSOLUTETRANSLATION};
use crate::pd_guns::data::{AnimMeta, Material, RawNode};
use crate::pd_guns::model::euler_quat;
use crate::pd_guns::pdmtx;

use super::gfx::{Blend, Gfx};

pub const SKEL_CHR: i32 = 0x09;
pub const SKEL_HEAD: i32 = 0x0d;
pub const SKEL_HUDPIECE: i32 = 0x2a;

const MODELNODETYPE_0100: u32 = 0x0100;
const MODELNODETYPE_0200: u32 = 0x0200;

pub fn models_dir() -> PathBuf {
    super::assets_dir().join("models")
}

// ─── file format ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Header {
    name: String,
    nummatrices: usize,
    skel: Option<i64>,
    nodes: Vec<RawNode>,
    parts: HashMap<String, usize>,
    materials: Vec<Material>,
    textures: HashMap<String, TexInfo>,
    chrinfo: Option<ChrInfoRo>,
    batches: Vec<BatchHead>,
}

#[derive(Deserialize)]
struct ChrInfoRo {
    animpart: u16,
    mtx: i16,
}

#[derive(Deserialize)]
struct BatchHead {
    node: usize,
    material: usize,
    nverts: usize,
    nidx: usize,
}

#[derive(Deserialize, Clone)]
pub struct TexInfo {
    pub file: String,
    #[serde(default)]
    pub levels: Option<u32>,
}

#[derive(Clone, Copy)]
pub struct Vert {
    pub pos: Vec3,
    pub mtx: u16,
    pub uv: [f32; 2],
    pub c: [u8; 4],
    pub flags: u8,
}

pub struct MBatch {
    pub node: usize,
    pub material: usize,
    pub verts: Vec<Vert>,
    pub idx: Vec<u16>,
}

#[derive(Clone, Debug)]
pub enum Kind {
    ChrInfo { animpart: u16, mtx: i16 },
    Position { pos: Vec3, animpart: u16, mtx: [i16; 3], flags: u32 },
    PositionHeld { pos: Vec3, mtx: i16 },
    Toggle,
    Distance { near: f32, far: f32 },
    HeadSpot,
    Dl,
    GunDl,
    Other,
}

pub struct MNode {
    pub kind: Kind,
    pub parent: Option<usize>,
    pub cull_exit: Option<Cull>,
    pub batches: Vec<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cull {
    None,
    Back,
    Front,
    Both,
}

fn parse_cull(s: &str) -> Option<Cull> {
    match s {
        "none" => Some(Cull::None),
        "back" => Some(Cull::Back),
        "front" => Some(Cull::Front),
        "both" => Some(Cull::Both),
        _ => None,
    }
}

/// A loaded `modeldef`.
pub struct MDef {
    pub name: String,
    pub skel: i32,
    pub nummatrices: usize,
    pub nodes: Vec<MNode>,
    pub parts: HashMap<i32, usize>,
    pub materials: Vec<Material>,
    pub textures: HashMap<u32, TexInfo>,
    pub batches: Vec<MBatch>,
    /// `modeldef_find_bbox_rodata`: the first BBOX node's box.
    pub bbox: Option<[f32; 6]>,
}

impl MDef {
    pub fn load(stem: &str) -> Result<MDef, String> {
        let path = models_dir().join(format!("{stem}.pdm"));
        let data = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if data.len() < 8 || &data[..4] != b"PDM1" {
            return Err(format!("{}: not a PDM1 file", path.display()));
        }
        let hl = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let head: Header = serde_json::from_slice(&data[8..8 + hl]).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut off = 8 + hl;
        let rd_f = |o: usize| f32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        let mut batches = Vec::with_capacity(head.batches.len());
        for b in &head.batches {
            let mut verts = Vec::with_capacity(b.nverts);
            for _ in 0..b.nverts {
                let o = off;
                verts.push(Vert {
                    pos: Vec3::new(rd_f(o), rd_f(o + 4), rd_f(o + 8)),
                    mtx: u16::from_le_bytes([data[o + 12], data[o + 13]]),
                    uv: [rd_f(o + 14), rd_f(o + 18)],
                    c: [data[o + 22], data[o + 23], data[o + 24], data[o + 25]],
                    flags: data[o + 26],
                });
                off += 28;
            }
            let mut idx = Vec::with_capacity(b.nidx);
            for _ in 0..b.nidx {
                idx.push(u16::from_le_bytes([data[off], data[off + 1]]));
                off += 2;
            }
            batches.push(MBatch { node: b.node, material: b.material, verts, idx });
        }
        let mut nodes: Vec<MNode> = head
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let kind = match n.kind.as_str() {
                    "chrinfo" => match (&head.chrinfo, i) {
                        (Some(ci), 0) => Kind::ChrInfo { animpart: ci.animpart, mtx: ci.mtx },
                        _ => Kind::Other,
                    },
                    "position" => Kind::Position {
                        pos: Vec3::from(n.pos.unwrap_or([0.0; 3])),
                        animpart: n.animpart.unwrap_or(0),
                        mtx: n.mtx.unwrap_or([0, -1, -1]),
                        flags: n.flags.unwrap_or(0),
                    },
                    "positionheld" => Kind::PositionHeld { pos: Vec3::from(n.pos.unwrap_or([0.0; 3])), mtx: n.mtx.map_or(0, |m| m[0]) },
                    "toggle" => Kind::Toggle,
                    "distance" => Kind::Distance { near: n.near.unwrap_or(0.0), far: n.far.unwrap_or(0.0) },
                    "headspot" => Kind::HeadSpot,
                    "dl" => Kind::Dl,
                    "gundl" => Kind::GunDl,
                    _ => Kind::Other,
                };
                MNode {
                    kind,
                    parent: if n.parent >= 0 { Some(n.parent as usize) } else { None },
                    cull_exit: n.cull_exit.as_deref().and_then(parse_cull),
                    batches: Vec::new(),
                }
            })
            .collect();
        for (bi, b) in batches.iter().enumerate() {
            if let Some(n) = nodes.get_mut(b.node) {
                n.batches.push(bi);
            }
        }
        Ok(MDef {
            name: head.name,
            skel: head.skel.unwrap_or(0) as i32,
            nummatrices: head.nummatrices.max(1),
            parts: head.parts.iter().filter_map(|(k, v)| k.parse::<i32>().ok().map(|p| (p, *v))).collect(),
            bbox: head.nodes.iter().find_map(|n| n.bbox),
            nodes,
            materials: head.materials,
            textures: head.textures.iter().filter_map(|(k, v)| k.parse::<u32>().ok().map(|id| (id, v.clone()))).collect(),
            batches,
        })
    }

    pub fn get_part(&self, partnum: i32) -> Option<usize> {
        self.parts.get(&partnum).copied()
    }

    /// `model_find_node_mtx_index(node, which)` (model.c:123).
    pub fn find_node_mtx_index(&self, node: usize, which: u32) -> Option<usize> {
        let mut cur = Some(node);
        while let Some(i) = cur {
            match &self.nodes[i].kind {
                Kind::ChrInfo { mtx, .. } => return (*mtx >= 0).then_some(*mtx as usize),
                Kind::Position { mtx, .. } => {
                    let m = mtx[if which == MODELNODETYPE_0200 { 2 } else if which == MODELNODETYPE_0100 { 1 } else { 0 }];
                    return (m >= 0).then_some(m as usize);
                }
                Kind::PositionHeld { mtx, .. } => return (*mtx >= 0).then_some(*mtx as usize),
                _ => {}
            }
            cur = self.nodes[i].parent;
        }
        None
    }

    /// `body_calculate_head_offset`'s vertex pass (body.c): every
    /// `MODELNODETYPE_DL` vertex and the bbox move up by `offset`.
    pub fn with_head_offset(&self, offset: f32) -> MDef {
        let batches = self
            .batches
            .iter()
            .map(|b| {
                let shift = matches!(self.nodes[b.node].kind, Kind::Dl);
                MBatch {
                    node: b.node,
                    material: b.material,
                    verts: b.verts.iter().map(|v| Vert { pos: if shift { v.pos + Vec3::Y * offset } else { v.pos }, ..*v }).collect(),
                    idx: b.idx.clone(),
                }
            })
            .collect();
        MDef {
            name: self.name.clone(),
            skel: self.skel,
            nummatrices: self.nummatrices,
            nodes: self
                .nodes
                .iter()
                .map(|n| MNode { kind: n.kind.clone(), parent: n.parent, cull_exit: n.cull_exit, batches: n.batches.clone() })
                .collect(),
            parts: self.parts.clone(),
            materials: self.materials.clone(),
            textures: self.textures.clone(),
            batches,
            bbox: self.bbox.map(|mut b| {
                b[2] += offset;
                b[3] += offset;
                b
            }),
        }
    }
}

// ─── textures ────────────────────────────────────────────────────────────────

pub struct MipTex {
    /// Level 0 first; each level RGBA 0..1.
    pub levels: Vec<(usize, usize, Vec<[f32; 4]>)>,
}

impl MipTex {
    fn load(file: &str, levels: Option<u32>) -> Option<MipTex> {
        let img = image::open(models_dir().join("tex").join(file)).ok()?.to_rgba8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let px: Vec<[f32; 4]> = img.pixels().map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0]).collect();
        let mut out = vec![(w, h, px)];
        let want = levels.unwrap_or(1).clamp(1, 8) as usize;
        while out.len() < want.max(6) {
            let (pw, ph, prev) = out.last().unwrap();
            let (pw, ph) = (*pw, *ph);
            if pw <= 1 && ph <= 1 {
                break;
            }
            let (nw, nh) = ((pw / 2).max(1), (ph / 2).max(1));
            let mut next = vec![[0.0f32; 4]; nw * nh];
            for y in 0..nh {
                for x in 0..nw {
                    let mut acc = [0.0f32; 4];
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(pw - 1);
                        let sy = (y * 2 + dy).min(ph - 1);
                        let p = prev[sy * pw + sx];
                        for i in 0..4 {
                            acc[i] += p[i] * 0.25;
                        }
                    }
                    next[y * nw + x] = acc;
                }
            }
            out.push((nw, nh, next));
        }
        Some(MipTex { levels: out })
    }

    fn addr(i: i32, n: usize, mode: u8) -> usize {
        let n = n as i32;
        match mode {
            1 => i.clamp(0, n - 1) as usize,
            2 => {
                let m = i.rem_euclid(2 * n);
                (if m >= n { 2 * n - 1 - m } else { m }) as usize
            }
            _ => i.rem_euclid(n) as usize,
        }
    }

    /// One level, `s,t` in that level's texels.
    fn sample(&self, level: usize, s: f32, t: f32, cms: u8, cmt: u8, bilerp: bool) -> [f32; 4] {
        let (w, h, px) = &self.levels[level.min(self.levels.len() - 1)];
        let texel = |x: i32, y: i32| px[Self::addr(y, *h, cmt) * w + Self::addr(x, *w, cms)];
        if !bilerp {
            return texel(s.floor() as i32, t.floor() as i32);
        }
        // The RDP's three-point filter.
        let (s, t) = (s - 0.5, t - 0.5);
        let (s0, t0) = (s.floor(), t.floor());
        let (fs, ft) = (s - s0, t - t0);
        let (s0, t0) = (s0 as i32, t0 as i32);
        let mut out = [0.0; 4];
        if fs + ft < 1.0 {
            let (a, b, c) = (texel(s0, t0), texel(s0 + 1, t0), texel(s0, t0 + 1));
            for i in 0..4 {
                out[i] = a[i] + fs * (b[i] - a[i]) + ft * (c[i] - a[i]);
            }
        } else {
            let (d, b, c) = (texel(s0 + 1, t0 + 1), texel(s0 + 1, t0), texel(s0, t0 + 1));
            for i in 0..4 {
                out[i] = d[i] + (1.0 - fs) * (c[i] - d[i]) + (1.0 - ft) * (b[i] - d[i]);
            }
        }
        out
    }
}

// ─── the store ───────────────────────────────────────────────────────────────

/// Model files, textures and the two menu animations, loaded on first use.
pub struct ModelStore {
    index: HashMap<u32, String>,
    defs: HashMap<String, Arc<MDef>>,
    textures: HashMap<String, Option<Arc<MipTex>>>,
    pub bank: AnimBank,
    pub error: Option<String>,
}

impl ModelStore {
    pub fn load() -> ModelStore {
        let dir = models_dir();
        let mut error = None;
        let index: HashMap<u32, String> = std::fs::read_to_string(dir.join("index.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<HashMap<String, String>>(&t).ok())
            .map(|m| m.into_iter().filter_map(|(k, v)| k.parse().ok().map(|n| (n, v))).collect())
            .unwrap_or_else(|| {
                error = Some(format!("{}: missing index.json (run tools/pd-assets/pd_menu_models.py)", dir.display()));
                HashMap::new()
            });
        let bank = std::fs::read_to_string(dir.join("anims").join("anims.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<HashMap<String, AnimMeta>>(&t).ok())
            .and_then(|metas| AnimBank::load(&dir.join("anims"), &metas).ok())
            .unwrap_or(AnimBank { anims: HashMap::new() });
        ModelStore { index, defs: HashMap::new(), textures: HashMap::new(), bank, error }
    }

    /// `modeldef_load(filenum)`.
    pub fn def(&mut self, filenum: u32) -> Option<Arc<MDef>> {
        let stem = self.index.get(&filenum)?.clone();
        if let Some(d) = self.defs.get(&stem) {
            return Some(d.clone());
        }
        match MDef::load(&stem) {
            Ok(d) => {
                let d = Arc::new(d);
                self.defs.insert(stem, d.clone());
                Some(d)
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }

    fn texture(&mut self, info: &TexInfo) -> Option<Arc<MipTex>> {
        self.textures.entry(info.file.clone()).or_insert_with(|| MipTex::load(&info.file, info.levels).map(Arc::new)).clone()
    }
}

// ─── an instance ─────────────────────────────────────────────────────────────

/// `menumodel->bodymodel` + `bodyanim` + the attached head.
pub struct Inst {
    pub body: Arc<MDef>,
    pub head: Option<Arc<MDef>>,
    /// Toggle / distance visibility per body node, then per head node.
    pub vis: Vec<bool>,
    pub head_vis: Vec<bool>,
    pub anim: Anim,
    pub chrinfo: ChrInfo,
    /// `model->scale`.
    pub scale: f32,
    pub matrices: Vec<Mat4>,
    /// The hudpiece's scrolling "liquid" texture: added to every vertex `s` of
    /// MODELPART_HUDPIECE_0000, in S10.5 units (menu.c:2254).
    pub hud_s: i32,
}

impl Inst {
    pub fn new(body: Arc<MDef>, head: Option<Arc<MDef>>) -> Inst {
        let vis = body.nodes.iter().map(|n| !matches!(n.kind, Kind::Distance { .. })).collect();
        let head_vis = head.as_ref().map_or(Vec::new(), |h| h.nodes.iter().map(|n| !matches!(n.kind, Kind::Distance { .. })).collect());
        let n = body.nummatrices;
        Inst { body, head, vis, head_vis, anim: Anim::default(), chrinfo: ChrInfo::default(), scale: 1.0, matrices: vec![Mat4::IDENTITY; n], hud_s: 0 }
    }

    /// Set a TOGGLE part's visibility (`rwdata->toggle.visible`) on the body
    /// model (`head == false`) or the attached head.
    pub fn set_toggle(&mut self, head: bool, partnum: i32, visible: bool) {
        let (def, vis) = if head {
            match &self.head {
                Some(h) => (h.clone(), &mut self.head_vis),
                None => return,
            }
        } else {
            (self.body.clone(), &mut self.vis)
        };
        if let Some(n) = def.get_part(partnum) {
            if matches!(def.nodes[n].kind, Kind::Toggle) {
                vis[n] = visible;
            }
        }
    }

    pub fn anim_ctx<'a>(&'a mut self, bank: &'a AnimBank) -> (AnimCtx<'a>, &'a mut Anim) {
        let chr = if let Some(Kind::ChrInfo { animpart, .. }) = self.body.nodes.first().map(|n| &n.kind) {
            Some((&mut self.chrinfo, *animpart as usize))
        } else {
            None
        };
        (AnimCtx { bank, scale: self.scale, chrinfo: chr, merging_enabled: true }, &mut self.anim)
    }

    fn walk_reaches(def: &MDef, vis: &[bool], node: usize) -> bool {
        let mut cur = def.nodes[node].parent;
        while let Some(i) = cur {
            if matches!(def.nodes[i].kind, Kind::Toggle | Kind::Distance { .. }) && !vis[i] {
                return false;
            }
            cur = def.nodes[i].parent;
        }
        true
    }

    /// `model_set_matrices_with_anim` (model.c:1568) with `renderdata.rendermtx`:
    /// every joint, then the distance relations (`model_update_distance_relations`).
    pub fn set_matrices_with_anim(&mut self, rendermtx: &Mat4, bank: &AnimBank) {
        let def = self.body.clone();
        for i in 0..def.nodes.len() {
            if !Self::walk_reaches(&def, &self.vis, i) {
                continue;
            }
            match def.nodes[i].kind.clone() {
                Kind::ChrInfo { animpart, mtx } => self.update_chr_node(&def, i, animpart as usize, mtx, rendermtx, bank),
                Kind::Position { pos, animpart, mtx, flags } => self.update_position_node(&def, i, pos, animpart as usize, mtx, flags, rendermtx, bank),
                Kind::PositionHeld { pos, mtx } => {
                    let parent = self.parent_mtx(&def, i, rendermtx);
                    let local = Mat4::from_translation(pos);
                    if let Some(slot) = self.matrices.get_mut(mtx as usize) {
                        *slot = match parent {
                            Some(p) => pdmtx::mul(&p, &local),
                            None => local,
                        };
                    }
                }
                Kind::Distance { near, far } => {
                    let d = self.distance_of(&def, i);
                    self.vis[i] = (d > near * self.scale || near == 0.0) && d <= far * self.scale;
                }
                _ => {}
            }
        }
        if let Some(head) = self.head.clone() {
            // The head hangs off the headspot, whose parent joint holds its matrix.
            let spot = def.get_part(4).filter(|&n| matches!(def.nodes[n].kind, Kind::HeadSpot));
            let spot_mtx = spot.and_then(|n| def.find_node_mtx_index(n, 0)).map(|m| self.matrices[m]);
            for i in 0..head.nodes.len() {
                if let Kind::Distance { near, far } = head.nodes[i].kind {
                    let d = spot_mtx.map_or(0.0, |m| -m.w_axis.z);
                    self.head_vis[i] = (d > near * self.scale || near == 0.0) && d <= far * self.scale;
                }
            }
        }
    }

    /// `-mtx->m[3][2] * cam_get_lod_scale_z()` (the LOD scale is 1 at 60°).
    fn distance_of(&self, def: &MDef, node: usize) -> f32 {
        match def.find_node_mtx_index(node, 0) {
            Some(m) => -self.matrices[m].w_axis.z,
            None => 0.0,
        }
    }

    fn parent_mtx(&self, def: &MDef, node: usize, rendermtx: &Mat4) -> Option<Mat4> {
        match def.nodes[node].parent {
            Some(p) => def.find_node_mtx_index(p, 0).map(|i| self.matrices[i]),
            None => Some(*rendermtx),
        }
    }

    fn rot_trans_scale(&self, bank: &AnimBank, animpart: usize, second: bool) -> Option<(Vec3, Vec3, Vec3)> {
        let a = &self.anim;
        let (num, fa, fb, frac) = if second { (a.animnum2, a.frame2a, a.frame2b, a.frac2) } else { (a.animnum, a.framea, a.frameb, a.frac) };
        let ad = bank.get(num)?;
        let (mut r, t, s) = ad.rot_translate_scale(animpart, fa);
        if frac != 0.0 {
            let (r2, _, _) = ad.rot_translate_scale(animpart, fb);
            r = pdmtx::tween_rot(r, r2, frac);
        }
        Some((r, t, s))
    }

    /// `model_update_chr_node_mtx` (model.c:726).
    fn update_chr_node(&mut self, def: &MDef, node: usize, animpart: usize, slot: i16, rendermtx: &Mat4, bank: &AnimBank) {
        if slot < 0 {
            return;
        }
        let parent = self.parent_mtx(def, node, rendermtx);
        let rot1 = self.rot_trans_scale(bank, animpart, false).map_or(Vec3::ZERO, |r| r.0);
        let abs = bank.flags(self.anim.animnum) & ANIMFLAG_ABSOLUTETRANSLATION != 0;
        let rotm = if self.anim.fracmerge != 0.0 {
            let rot3 = self.rot_trans_scale(bank, animpart, true).map_or(Vec3::ZERO, |r| r.0);
            let mut q3 = if abs && bank.flags(self.anim.animnum2) & ANIMFLAG_ABSOLUTETRANSLATION == 0 {
                let m = pdmtx::mul(&pdmtx::load_y_rotation(self.chrinfo.yrot), &pdmtx::load_rotation(rot3));
                Quat::from_mat4(&m)
            } else {
                euler_quat(rot3)
            };
            let q1 = euler_quat(rot1);
            if q1.dot(q3) < 0.0 {
                q3 = -q3;
            }
            Mat4::from_quat(q1.slerp(q3, self.anim.fracmerge))
        } else {
            pdmtx::load_rotation(rot1)
        };
        let ci = &self.chrinfo;
        let sp198 = if abs {
            Mat4::from_translation(ci.pos)
        } else {
            let mut yrot = ci.yrot;
            if ci.unk18 != 0.0 {
                yrot = pdmtx::tween_rot_axis(yrot, ci.unk1c, ci.unk18);
            }
            let mut m = pdmtx::load_y_rotation(yrot);
            pdmtx::set_translation(&mut m, ci.pos);
            m
        };
        let mut sp158 = pdmtx::mul(&sp198, &rotm);
        if self.scale != 1.0 {
            // mtx00015f4c: the 3×3 only.
            for c in [&mut sp158.x_axis, &mut sp158.y_axis, &mut sp158.z_axis] {
                c.x *= self.scale;
                c.y *= self.scale;
                c.z *= self.scale;
            }
        }
        self.matrices[slot as usize] = match parent {
            Some(p) => pdmtx::mul(&p, &sp158),
            None => sp158,
        };
    }

    /// `model_update_position_node_mtx` (model.c:1052) +
    /// `model_position_joint_using_vec_rot` / `_quat_rot` (model.c:834, :954).
    #[allow(clippy::too_many_arguments)]
    fn update_position_node(&mut self, def: &MDef, node: usize, rodata_pos: Vec3, animpart: usize, mtx: [i16; 3], nodeflags: u32, rendermtx: &Mat4, bank: &AnimBank) {
        let is_root = node == 0;
        let parent = self.parent_mtx(def, node, rendermtx);
        let a = &self.anim;
        let mut rot1 = Vec3::ZERO;
        let mut translate1 = Vec3::ZERO;
        let mut scale1 = Vec3::ONE;
        let mut sp128 = false;
        if a.animnum != 0 {
            if let Some(ad) = bank.get(a.animnum) {
                sp128 = ad.flags & ANIMFLAG_ABSOLUTETRANSLATION != 0 && is_root;
                let (r, t, s) = ad.rot_translate_scale(animpart, a.framea);
                rot1 = r;
                translate1 = t;
                scale1 = s;
                if a.frac != 0.0 {
                    let (r2, t2, _) = ad.rot_translate_scale(animpart, a.frameb);
                    rot1 = pdmtx::tween_rot(rot1, r2, a.frac);
                    if sp128 {
                        translate1 += (t2 - translate1) * a.frac;
                    }
                }
            }
        }
        let animscale = a.animscale;
        let pos = if sp128 {
            translate1
        } else if translate1 != Vec3::ZERO {
            let mut t = translate1 * animscale;
            if !is_root {
                t += rodata_pos;
            }
            t
        } else if !is_root {
            rodata_pos
        } else {
            translate1
        };
        if a.fracmerge != 0.0 {
            let rot3 = self.rot_trans_scale(bank, animpart, true).map_or(Vec3::ZERO, |r| r.0);
            let q1 = euler_quat(rot1);
            let mut q3 = euler_quat(rot3);
            if q1.dot(q3) < 0.0 {
                q3 = -q3;
            }
            let q = q1.slerp(q3, a.fracmerge);
            self.position_joint(mtx, nodeflags, parent, JointRot::Quat(q), pos, false, scale1);
        } else {
            self.position_joint(mtx, nodeflags, parent, JointRot::Euler(rot1), pos, sp128, scale1);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn position_joint(&mut self, mtx: [i16; 3], nodeflags: u32, parent: Option<Mat4>, rot: JointRot, pos: Vec3, allowscale: bool, s: Vec3) {
        let q = match rot {
            JointRot::Euler(r) => euler_quat(r),
            JointRot::Quat(q) => q,
        };
        let mut local = match rot {
            JointRot::Euler(r) => pdmtx::load_rotation_translation(pos, r),
            JointRot::Quat(q) => Mat4::from_rotation_translation(q, pos),
        };
        if allowscale && self.scale != 1.0 {
            pdmtx::scale3(&mut local, self.scale);
        }
        if s.x != 1.0 {
            local.x_axis *= s.x;
        }
        if s.y != 1.0 {
            local.y_axis *= s.y;
        }
        if s.z != 1.0 {
            local.z_axis *= s.z;
        }
        let place = |m: Mat4| match parent {
            Some(p) => pdmtx::mul(&p, &m),
            None => m,
        };
        if mtx[0] >= 0 {
            self.matrices[mtx[0] as usize] = place(local);
        }
        if nodeflags & MODELNODETYPE_0100 != 0 && mtx[1] >= 0 {
            // quaternion0f097518(q, 0.5): half the joint's rotation, the
            // elbow / knee skin between the two bones.
            let half = half_rotation(q);
            self.matrices[mtx[1] as usize] = place(Mat4::from_rotation_translation(half, pos));
        }
        if nodeflags & MODELNODETYPE_0200 != 0 && mtx[2] >= 0 {
            let full = std::f32::consts::TAU;
            let mut roty = match rot {
                JointRot::Euler(r) => r.y,
                JointRot::Quat(q) => 2.0 * q.w.clamp(-1.0, 1.0).acos(),
            };
            roty = if roty < std::f32::consts::PI { roty * 0.5 } else { full - (full - roty) * 0.5 };
            let mut m = pdmtx::load_y_rotation(roty);
            if roty >= std::f32::consts::PI {
                roty = full - roty;
            }
            let k = if roty < 51f32.to_radians() { (roty.sin() / roty.cos() + 1.0).sqrt() } else { 1.5 };
            // mtx00015edc: scale m[2] (the z column).
            m.z_axis.x *= k;
            m.z_axis.y *= k;
            m.z_axis.z *= k;
            pdmtx::set_translation(&mut m, pos);
            self.matrices[mtx[2] as usize] = place(m);
        }
    }
}

enum JointRot {
    Euler(Vec3),
    Quat(Quat),
}

/// `quaternion0f097518(q, 0.5)`: slerp from the identity (sign-matched) to `q`.
fn half_rotation(q: Quat) -> Quat {
    let t = 0.5f32;
    let (mut w0, mut sign) = (q.w, 1.0f32);
    if w0 < 0.0 {
        w0 = -w0;
        sign = -1.0;
    }
    let (w, k) = if w0 < -0.99999 || w0 > 0.99999 {
        (q.w * t + (1.0 - t) * sign, t)
    } else {
        let th = w0.acos();
        let s = th.sin();
        let k1 = (t * th).sin() / s;
        let k0 = ((1.0 - t) * th).sin() / s;
        (q.w * k1 + k0 * sign, k1)
    };
    Quat::from_xyzw(q.x * k, q.y * k, q.z * k, w).normalize()
}

// ─── drawing ─────────────────────────────────────────────────────────────────

/// `Lights1` + `LookAt` for the model pass.
pub struct Lighting {
    pub ambient: f32,
    pub diffuse: f32,
    /// Raw light direction / 127, eye space.
    pub dir: Vec3,
    pub lookat_x: Vec3,
    pub lookat_y: Vec3,
}

/// `var80071468` (menu.c:1714): `gdSPDefLights1(0x96,0x96,0x96, 0xff,0xff,0xff, 0xb2,0x4d,0x2e)`.
pub fn menu_lights() -> Lighting {
    let sb = |b: u8| b as i8 as f32 / 127.0;
    Lighting { ambient: 150.0, diffuse: 255.0, dir: Vec3::new(sb(0xb2), sb(0x4d), sb(0x2e)), lookat_x: Vec3::X, lookat_y: Vec3::Y }
}

/// Where the projected model lands: the viewport, in framebuffer pixels.
pub struct View {
    pub proj: Mat4,
    pub vp: [f32; 4],
    pub near: f32,
}

#[derive(Clone, Copy, Default)]
struct PV {
    eye: Vec4,
    clip: Vec4,
    st: [f32; 2],
    shade: [f32; 4],
}

impl PV {
    fn lerp(a: &PV, b: &PV, t: f32) -> PV {
        let l = |x: f32, y: f32| x + (y - x) * t;
        PV {
            eye: a.eye + (b.eye - a.eye) * t,
            clip: a.clip + (b.clip - a.clip) * t,
            st: [l(a.st[0], b.st[0]), l(a.st[1], b.st[1])],
            shade: [l(a.shade[0], b.shade[0]), l(a.shade[1], b.shade[1]), l(a.shade[2], b.shade[2]), l(a.shade[3], b.shade[3])],
        }
    }
}

struct Mat<'a> {
    m: &'a Material,
    tex: Option<Arc<MipTex>>,
    shift: [f32; 2],
    ul: [f32; 2],
    env: [f32; 4],
    prim: [f32; 4],
}

fn shift_scale(s: u8) -> f32 {
    match s {
        0 => 1.0,
        1..=10 => 1.0 / (1u32 << s) as f32,
        _ => (1u32 << (16 - s as u32)) as f32,
    }
}

fn rgba_f(c: [u8; 4]) -> [f32; 4] {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, c[3] as f32 / 255.0]
}

impl ModelStore {
    /// `model_render` for an instance whose matrices are set: the body's node
    /// tree in order, the head's in place of the HEADSPOT.
    pub fn render(&mut self, gfx: &mut Gfx, inst: &Inst, view: &View, lights: &Lighting) {
        let mut cull = Cull::Back;
        let body = inst.body.clone();
        for i in 0..body.nodes.len() {
            if !Inst::walk_reaches(&body, &inst.vis, i) {
                continue;
            }
            if matches!(body.nodes[i].kind, Kind::HeadSpot) {
                if let Some(head) = inst.head.clone() {
                    for j in 0..head.nodes.len() {
                        if Inst::walk_reaches(&head, &inst.head_vis, j) {
                            self.render_node(gfx, &head, j, inst, view, lights, &mut cull);
                        }
                    }
                }
                continue;
            }
            self.render_node(gfx, &body, i, inst, view, lights, &mut cull);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_node(&mut self, gfx: &mut Gfx, def: &MDef, node: usize, inst: &Inst, view: &View, lights: &Lighting, cull: &mut Cull) {
        let n = &def.nodes[node];
        let hud_liquid = def.skel == SKEL_HUDPIECE && def.get_part(0) == Some(node);
        for &bi in &n.batches {
            let b = &def.batches[bi];
            let m = &def.materials[b.material];
            let bcull = parse_cull(&m.cull).unwrap_or(*cull);
            let mut mat = Mat { m, tex: None, shift: [1.0, 1.0], ul: [0.0, 0.0], env: rgba_f(m.env.unwrap_or([255; 4])), prim: rgba_f(m.prim) };
            if let Some(t) = &m.texture {
                if let Some(info) = def.textures.get(&t.id) {
                    mat.tex = self.texture(info);
                }
                mat.shift = [shift_scale(t.shifts), shift_scale(t.shiftt)];
                mat.ul = [t.uls, t.ult];
            }
            // Vertex stage.
            let pv: Vec<PV> = b
                .verts
                .iter()
                .map(|v| {
                    let mtx = inst.matrices.get(v.mtx as usize).copied().unwrap_or(Mat4::IDENTITY);
                    let eye = mtx * v.pos.extend(1.0);
                    let mut st = v.uv;
                    if hud_liquid {
                        st[0] += inst.hud_s as f32 / 32.0;
                    }
                    let shade = if v.flags & 1 != 0 {
                        let nrm = Vec3::new(v.c[0] as i8 as f32, v.c[1] as i8 as f32, v.c[2] as i8 as f32);
                        let m3t = Mat3::from_mat4(mtx).transpose();
                        let coeffs = (m3t * lights.dir).normalize_or_zero();
                        let inten = nrm.dot(coeffs) / 127.0;
                        let c = lights.ambient + if inten > 0.0 { inten * lights.diffuse } else { 0.0 };
                        let c = c.min(255.0) / 255.0;
                        if v.flags & 2 != 0 {
                            let cx = (m3t * lights.lookat_x).normalize_or_zero();
                            let cy = (m3t * lights.lookat_y).normalize_or_zero();
                            let mut dx = (nrm.dot(cx) / 127.0).clamp(-1.0, 1.0);
                            let mut dy = (nrm.dot(cy) / 127.0).clamp(-1.0, 1.0);
                            if m.texgen_linear {
                                dx = (-dx).acos() / 4.0;
                                dy = (-dy).acos() / 4.0;
                            } else {
                                dx = (dx + 1.0) / 4.0;
                                dy = (dy + 1.0) / 4.0;
                            }
                            st = [dx * v.uv[0], dy * v.uv[1]];
                        }
                        [c, c, c, v.c[3] as f32 / 255.0]
                    } else {
                        [v.c[0] as f32 / 255.0, v.c[1] as f32 / 255.0, v.c[2] as f32 / 255.0, v.c[3] as f32 / 255.0]
                    };
                    PV { eye, clip: view.proj * eye, st, shade }
                })
                .collect();
            for tri in b.idx.chunks_exact(3) {
                let (a, bb, c) = (pv[tri[0] as usize], pv[tri[1] as usize], pv[tri[2] as usize]);
                raster_clipped(gfx, [a, bb, c], &mat, view, bcull);
            }
        }
        if let Some(c) = n.cull_exit {
            *cull = c;
        }
    }
}

/// Clip against the near plane (eye `z = −near`), then rasterise.
fn raster_clipped(gfx: &mut Gfx, v: [PV; 3], mat: &Mat, view: &View, cull: Cull) {
    let inside = |p: &PV| p.eye.z <= -view.near;
    if v.iter().all(inside) {
        raster(gfx, v, mat, view, cull);
        return;
    }
    if !v.iter().any(inside) {
        return;
    }
    let mut poly: Vec<PV> = Vec::with_capacity(4);
    for i in 0..3 {
        let (a, b) = (&v[i], &v[(i + 1) % 3]);
        if inside(a) {
            poly.push(*a);
        }
        if inside(a) != inside(b) {
            let t = (-view.near - a.eye.z) / (b.eye.z - a.eye.z);
            poly.push(PV::lerp(a, b, t));
        }
    }
    for k in 1..poly.len().saturating_sub(1) {
        raster(gfx, [poly[0], poly[k], poly[k + 1]], mat, view, cull);
    }
}

struct SP {
    x: f32,
    y: f32,
    z: f32,
    iw: f32,
    s: f32,
    t: f32,
    c: [f32; 4],
}

fn raster(gfx: &mut Gfx, v: [PV; 3], mat: &Mat, view: &View, cull: Cull) {
    let [vx, vy, vw, vh] = view.vp;
    let sp: Vec<SP> = v
        .iter()
        .map(|p| {
            let iw = 1.0 / p.clip.w;
            let (nx, ny, nz) = (p.clip.x * iw, p.clip.y * iw, p.clip.z * iw);
            SP { x: vx + (nx + 1.0) * 0.5 * vw, y: vy + (1.0 - ny) * 0.5 * vh, z: nz, iw, s: p.st[0], t: p.st[1], c: p.shade }
        })
        .collect();
    let area = (sp[1].x - sp[0].x) * (sp[2].y - sp[0].y) - (sp[2].x - sp[0].x) * (sp[1].y - sp[0].y);
    if area.abs() < 1e-9 {
        return;
    }
    // Screen y points down: a front face (CCW, y up) has negative area here.
    let front = area < 0.0;
    match cull {
        Cull::Back if !front => return,
        Cull::Front if front => return,
        Cull::Both => return,
        _ => {}
    }
    let [sx1, sy1, sx2, sy2] = gfx.scissor;
    let minx = sp.iter().map(|p| p.x).fold(f32::INFINITY, f32::min).floor().max(sx1 as f32) as i32;
    let maxx = sp.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max).ceil().min(sx2 as f32) as i32;
    let miny = sp.iter().map(|p| p.y).fold(f32::INFINITY, f32::min).floor().max(sy1 as f32) as i32;
    let maxy = sp.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max).ceil().min(sy2 as f32) as i32;
    if minx >= maxx || miny >= maxy {
        return;
    }

    // Texture: tile coordinates are texels × 2^-shift − (uls, ult).
    let tex = mat.tex.as_deref();
    let t_cfg = mat.m.texture.as_ref();
    let tile = |s: f32, t: f32| (s * mat.shift[0] - mat.ul[0], t * mat.shift[1] - mat.ul[1]);
    // TRILERP LOD from the triangle's texel / pixel area ratio.
    let (lod_level, lod_frac) = match (tex, t_cfg) {
        (Some(tx), Some(tc)) if tc.mipmap && tx.levels.len() > 1 => {
            let st: Vec<(f32, f32)> = sp.iter().map(|p| tile(p.s, p.t)).collect();
            let tarea = ((st[1].0 - st[0].0) * (st[2].1 - st[0].1) - (st[2].0 - st[0].0) * (st[1].1 - st[0].1)).abs();
            let lod = (0.5 * (tarea / area.abs()).max(1e-9).log2()).clamp(0.0, (tx.levels.len() - 1) as f32);
            (lod.floor() as usize, lod.fract())
        }
        _ => (0, 0.0),
    };
    let bilerp = t_cfg.is_some_and(|t| t.linear);
    let (cms, cmt) = t_cfg.map_or((0, 0), |t| (t.cms, t.cmt));
    let two_cycle = mat.m.two_cycle;
    let cc = mat.m.combine;
    let xlu = mat.m.blend == "alpha";
    let alpha_test = mat.m.alpha_test.as_str();

    let edge = |a: &SP, b: &SP, px: f32, py: f32| (b.x - a.x) * (py - a.y) - (b.y - a.y) * (px - a.x);
    let sign = area.signum();
    let is_tl = |a: &SP, b: &SP| {
        let (dx, dy) = ((b.x - a.x) * sign, (b.y - a.y) * sign);
        (dy == 0.0 && dx > 0.0) || dy < 0.0
    };
    let tl = [is_tl(&sp[1], &sp[2]), is_tl(&sp[2], &sp[0]), is_tl(&sp[0], &sp[1])];
    let inv_area = 1.0 / area;
    let w = gfx.w;
    for py in miny..maxy {
        let fy = py as f32 + 0.5;
        for px in minx..maxx {
            let fx = px as f32 + 0.5;
            let w0 = edge(&sp[1], &sp[2], fx, fy) * inv_area;
            let w1 = edge(&sp[2], &sp[0], fx, fy) * inv_area;
            let w2 = edge(&sp[0], &sp[1], fx, fy) * inv_area;
            let ins = |w: f32, tl: bool| w > 0.0 || (w == 0.0 && tl);
            if !(ins(w0, tl[0]) && ins(w1, tl[1]) && ins(w2, tl[2])) {
                continue;
            }
            let idx = py as usize * w + px as usize;
            let z = w0 * sp[0].z + w1 * sp[1].z + w2 * sp[2].z;
            if mat.m.ztest {
                let limit = gfx.zb[idx] + if mat.m.decal { 1e-4 } else { 0.0 };
                if z > limit {
                    continue;
                }
            }
            let mut shade = [0.0f32; 4];
            for i in 0..4 {
                shade[i] = (w0 * sp[0].c[i] + w1 * sp[1].c[i] + w2 * sp[2].c[i]).clamp(0.0, 1.0);
            }
            let (mut t0, mut t1) = ([1.0f32; 4], [1.0f32; 4]);
            if let Some(tx) = tex {
                let iw = w0 * sp[0].iw + w1 * sp[1].iw + w2 * sp[2].iw;
                let s = (w0 * sp[0].s * sp[0].iw + w1 * sp[1].s * sp[1].iw + w2 * sp[2].s * sp[2].iw) / iw;
                let t = (w0 * sp[0].t * sp[0].iw + w1 * sp[1].t * sp[1].iw + w2 * sp[2].t * sp[2].iw) / iw;
                let (s, t) = tile(s, t);
                let (w0l, h0l, _) = &tx.levels[0];
                let at = |lv: usize| {
                    let (lw, lh, _) = &tx.levels[lv.min(tx.levels.len() - 1)];
                    tx.sample(lv, s * *lw as f32 / *w0l as f32, t * *lh as f32 / *h0l as f32, cms, cmt, bilerp)
                };
                t0 = at(lod_level);
                t1 = if lod_frac > 0.0 { at(lod_level + 1) } else { t0 };
            }
            let inp = Inputs { combined: [0.0; 4], t0, t1, shade, prim: mat.prim, env: mat.env, lod: lod_frac };
            let mut c = combine(&cc[0..8], &inp);
            if two_cycle {
                let inp2 = Inputs { combined: c, ..inp };
                c = combine(&cc[8..16], &inp2);
            }
            match alpha_test {
                "edge" => {
                    if c[3] > 0.19 {
                        c[3] = 1.0;
                    } else {
                        continue;
                    }
                }
                "threshold" => {
                    if c[3] < 8.0 / 256.0 {
                        continue;
                    }
                }
                _ => {}
            }
            if mat.m.zwrite {
                gfx.zb[idx] = z;
            }
            gfx.blend_px(px, py, c, if xlu { Blend::Xlu } else { Blend::Opaque });
        }
    }
}

#[derive(Clone, Copy)]
struct Inputs {
    combined: [f32; 4],
    t0: [f32; 4],
    t1: [f32; 4],
    shade: [f32; 4],
    prim: [f32; 4],
    env: [f32; 4],
    lod: f32,
}

/// One combiner cycle, fast3d's mux order `[a, b, c, d, Aa, Ab, Ac, Ad]`.
fn combine(m: &[u8], i: &Inputs) -> [f32; 4] {
    let rgb_src = |s: u8, allow_one: bool| -> [f32; 3] {
        let v = match s {
            0 => i.combined,
            1 => i.t0,
            2 => i.t1,
            3 => i.prim,
            4 => i.shade,
            5 => i.env,
            6 if allow_one => [1.0; 4],
            _ => [0.0; 4],
        };
        [v[0], v[1], v[2]]
    };
    let c_src = |s: u8| -> [f32; 3] {
        match s {
            0..=5 => rgb_src(s, false),
            7 => [i.combined[3]; 3],
            8 => [i.t0[3]; 3],
            9 => [i.t1[3]; 3],
            10 => [i.prim[3]; 3],
            11 => [i.shade[3]; 3],
            12 => [i.env[3]; 3],
            13 => [i.lod; 3],
            _ => [0.0; 3],
        }
    };
    let a_abd = |s: u8| -> f32 {
        match s {
            0 => i.combined[3],
            1 => i.t0[3],
            2 => i.t1[3],
            3 => i.prim[3],
            4 => i.shade[3],
            5 => i.env[3],
            6 => 1.0,
            _ => 0.0,
        }
    };
    let a_c = |s: u8| -> f32 {
        match s {
            0 => i.lod,
            1 => i.t0[3],
            2 => i.t1[3],
            3 => i.prim[3],
            4 => i.shade[3],
            5 => i.env[3],
            _ => 0.0,
        }
    };
    let (a, b, c, d) = (rgb_src(m[0], true), rgb_src(m[1], false), c_src(m[2]), rgb_src(m[3], true));
    let mut out = [0.0f32; 4];
    for k in 0..3 {
        out[k] = ((a[k] - b[k]) * c[k] + d[k]).clamp(0.0, 1.0);
    }
    out[3] = ((a_abd(m[4]) - a_abd(m[5])) * a_c(m[6]) + a_abd(m[7])).clamp(0.0, 1.0);
    out
}
