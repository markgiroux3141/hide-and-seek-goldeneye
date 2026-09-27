//! A Perfect Dark model instance: the node tree from the exported JSON, per-node
//! toggle state (PD's `rwdata`), and the matrix array `model_set_matrices_with_anim`
//! fills (`model.c:1568` → `model_update_matrices` → `model_update_position_node_mtx`).
//!
//! Only what the first-person gun, the hand model and the head-bob model need:
//! `POSITION`, `POSITIONHELD`, `CHRINFO` (head model), `TOGGLE`, `DISTANCE`, and the
//! drawable `GUNDL`/`DL`/`STARGUNFIRE`. Node order is PD's depth-first walk order,
//! so a parent always precedes its children.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Quat, Vec3};

use super::anim::{Anim, ChrInfo};
use super::animdata::{AnimBank, ANIMFLAG_ABSOLUTETRANSLATION};
use super::data::{Batch, Material, ModelFile};
use super::pdmtx;

#[derive(Clone, Debug)]
pub enum NodeKind {
    Position { pos: Vec3, animpart: u16, mtx: [i16; 3], flags: u32 },
    PositionHeld { pos: Vec3, mtx: i16 },
    ChrInfo { animpart: u16, mtx: i16 },
    Toggle,
    Distance { near: f32, far: f32 },
    Draw,
    StarGunfire { quads: Vec<[[i16; 3]; 4]> },
    Other,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub parent: Option<usize>,
    pub partnum: Option<i32>,
    /// The cull state this node leaves behind if its display list changed it.
    pub cull_exit: Option<Cull>,
    /// Batches (indices into `ModelDef::batches`) this node draws, in DL order.
    pub batches: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cull {
    None,
    Back,
    Front,
    Both,
}

impl Cull {
    pub fn parse(s: &str) -> Option<Cull> {
        match s {
            "none" => Some(Cull::None),
            "back" => Some(Cull::Back),
            "front" => Some(Cull::Front),
            "both" => Some(Cull::Both),
            _ => None,
        }
    }
}

/// The immutable definition (PD's `modeldef`).
pub struct ModelDef {
    pub name: String,
    pub nodes: Vec<Node>,
    pub parts: HashMap<i32, usize>,
    pub nummatrices: usize,
    pub materials: Vec<Material>,
    pub batches: Vec<Batch>,
    pub file: Option<ModelFile>,
    /// The first BBOX node's box (`model_find_bbox_rodata`): xmin, xmax, ymin,
    /// ymax, zmin, zmax in model units.
    pub bbox: Option<[f32; 6]>,
    /// Where this model's texture PNGs are, when not the guns' own folder (a
    /// stage's BG exports its textures beside it).
    pub tex_dir: Option<std::path::PathBuf>,
}

impl ModelDef {
    pub fn from_file(f: ModelFile) -> Self {
        let mut nodes: Vec<Node> = f
            .nodes
            .iter()
            .map(|n| {
                let kind = match n.kind.as_str() {
                    "position" => NodeKind::Position {
                        pos: Vec3::from(n.pos.unwrap_or([0.0; 3])),
                        animpart: n.animpart.unwrap_or(0),
                        mtx: n.mtx.unwrap_or([0, -1, -1]),
                        flags: n.flags.unwrap_or(0),
                    },
                    "positionheld" => NodeKind::PositionHeld {
                        pos: Vec3::from(n.pos.unwrap_or([0.0; 3])),
                        mtx: n.mtx.map_or(0, |m| m[0]),
                    },
                    "toggle" => NodeKind::Toggle,
                    "distance" => NodeKind::Distance { near: n.near.unwrap_or(0.0), far: n.far.unwrap_or(0.0) },
                    "gundl" | "dl" => NodeKind::Draw,
                    "stargunfire" => NodeKind::StarGunfire { quads: n.quads.clone().unwrap_or_default() },
                    _ => NodeKind::Other,
                };
                Node {
                    kind,
                    parent: if n.parent >= 0 { Some(n.parent as usize) } else { None },
                    partnum: n.partnum,
                    cull_exit: n.cull_exit.as_deref().and_then(Cull::parse),
                    batches: Vec::new(),
                }
            })
            .collect();
        for (bi, b) in f.batches.iter().enumerate() {
            if let Some(n) = nodes.get_mut(b.node) {
                n.batches.push(bi);
            }
        }
        let parts = f.parts.iter().filter_map(|(k, v)| k.parse::<i32>().ok().map(|p| (p, *v))).collect();
        let bbox = f.nodes.iter().find_map(|n| n.bbox);
        ModelDef {
            bbox,
            tex_dir: None,
            name: f.name.clone(),
            nodes,
            parts,
            nummatrices: f.nummatrices,
            materials: f.materials.clone(),
            batches: f.batches.clone(),
            file: Some(f),
        }
    }

    /// `model_get_part` (`model.c:327`).
    pub fn get_part(&self, partnum: i32) -> Option<usize> {
        self.parts.get(&partnum).copied()
    }

    /// `model_find_node_mtx_index(node, 0)` (`model.c:123`).
    pub fn find_node_mtx_index(&self, node: usize) -> Option<usize> {
        let mut cur = Some(node);
        while let Some(i) = cur {
            match &self.nodes[i].kind {
                NodeKind::Position { mtx, .. } => return Some(mtx[0] as usize),
                NodeKind::PositionHeld { mtx, .. } => return Some(*mtx as usize),
                NodeKind::ChrInfo { mtx, .. } => return Some(*mtx as usize),
                _ => {}
            }
            cur = self.nodes[i].parent;
        }
        None
    }

    pub fn is_root(&self, node: usize) -> bool {
        node == 0
    }
}

/// A live model: `struct model` (definition + rwdata + matrices).
pub struct Model {
    pub def: Arc<ModelDef>,
    /// Toggle / distance `visible` flags by node index (unused for other kinds).
    pub visible: Vec<bool>,
    pub matrices: Vec<Mat4>,
    /// `model->scale`.
    pub scale: f32,
    /// The CHRINFO root's rwdata (head model only).
    pub chrinfo: ChrInfo,
}

/// `g_ModelJointPositionedFunc` — called with each joint's matrix index and matrix
/// right after it is positioned, before its children inherit it.
pub type JointFn<'a> = &'a mut dyn FnMut(usize, &mut Mat4);

impl Model {
    /// `model_init` + `model_init_rw_data`: toggles start visible, distances hidden.
    pub fn new(def: Arc<ModelDef>) -> Self {
        let visible = def
            .nodes
            .iter()
            .map(|n| !matches!(n.kind, NodeKind::Distance { .. }))
            .collect();
        let n = def.nummatrices;
        Model { def, visible, matrices: vec![Mat4::IDENTITY; n], scale: 1.0, chrinfo: ChrInfo::default() }
    }

    /// Reset every toggle to visible — what `bgun_execute_model_cmd_list`'s
    /// compiled "toggle.visible = true" commands do at the top of every frame.
    pub fn reset_toggles(&mut self) {
        for (i, n) in self.def.nodes.iter().enumerate() {
            if matches!(n.kind, NodeKind::Toggle) {
                self.visible[i] = true;
            }
        }
    }

    /// `bgun_set_part_visible` on a part of THIS model (the toggle node).
    pub fn set_part_visible(&mut self, partnum: i32, visible: bool) {
        if let Some(node) = self.def.get_part(partnum) {
            if matches!(self.def.nodes[node].kind, NodeKind::Toggle) {
                self.visible[node] = visible;
            }
        }
    }

    /// A part's toggle state (false if the model has no such toggle part).
    pub fn part_visible(&self, partnum: i32) -> bool {
        self.def.get_part(partnum).is_some_and(|n| matches!(self.def.nodes[n].kind, NodeKind::Toggle) && self.visible[n])
    }

    /// True if every toggle/distance ancestor (and the node itself, if one) is on.
    pub fn node_visible(&self, node: usize) -> bool {
        let mut cur = Some(node);
        while let Some(i) = cur {
            match self.def.nodes[i].kind {
                NodeKind::Toggle | NodeKind::Distance { .. } => {
                    if !self.visible[i] {
                        return false;
                    }
                }
                _ => {}
            }
            cur = self.def.nodes[i].parent;
        }
        true
    }

    /// Whether the toggle above `node` (not the node itself) hides it — PD's
    /// matrix walk skips hidden subtrees (`model_update_matrices` follows
    /// `node->child`, which `model_apply_toggle_relations` nulls when hidden).
    fn walk_reaches(&self, node: usize) -> bool {
        let mut cur = self.def.nodes[node].parent;
        while let Some(i) = cur {
            if matches!(self.def.nodes[i].kind, NodeKind::Toggle | NodeKind::Distance { .. }) && !self.visible[i] {
                return false;
            }
            cur = self.def.nodes[i].parent;
        }
        true
    }

    /// `model_set_matrices_with_anim` (`model.c:1568`) with `renderdata.rendermtx`.
    pub fn set_matrices_with_anim(
        &mut self,
        rendermtx: &Mat4,
        anim: Option<&Anim>,
        bank: &AnimBank,
        mut joint_fn: Option<JointFn>,
    ) {
        let def = self.def.clone();
        for i in 0..def.nodes.len() {
            if !self.walk_reaches(i) {
                continue;
            }
            match def.nodes[i].kind.clone() {
                NodeKind::Position { pos, animpart, mtx, flags: _ } => {
                    self.update_position_node(i, pos, animpart as usize, mtx, rendermtx, anim, bank, &mut joint_fn);
                }
                NodeKind::PositionHeld { pos, mtx } => {
                    // model_update_position_held_node_mtx (model.c:1191)
                    let parent = self.parent_mtx(i, rendermtx);
                    let local = Mat4::from_translation(pos);
                    let m = match parent {
                        Some(p) => pdmtx::mul(&p, &local),
                        None => local,
                    };
                    if let Some(slot) = self.matrices.get_mut(mtx as usize) {
                        *slot = m;
                    }
                }
                NodeKind::ChrInfo { animpart, mtx } => {
                    if let Some(a) = anim {
                        self.update_chr_node(i, animpart as usize, mtx as usize, rendermtx, a, bank);
                    }
                }
                _ => {}
            }
        }
    }

    /// The matrix a node's parent chain provides, or `rendermtx` at the root —
    /// the `if (node->parent) ... else renderdata->rendermtx` head of every
    /// positioning function.
    fn parent_mtx(&self, node: usize, rendermtx: &Mat4) -> Option<Mat4> {
        match self.def.nodes[node].parent {
            Some(p) => self.def.find_node_mtx_index(p).map(|i| self.matrices[i]),
            None => Some(*rendermtx),
        }
    }

    /// `model_update_position_node_mtx` (`model.c:1052`).
    #[allow(clippy::too_many_arguments)]
    fn update_position_node(
        &mut self,
        node: usize,
        rodata_pos: Vec3,
        animpart: usize,
        mtx: [i16; 3],
        rendermtx: &Mat4,
        anim: Option<&Anim>,
        bank: &AnimBank,
        joint_fn: &mut Option<JointFn>,
    ) {
        let is_root = node == 0;
        let parent = self.parent_mtx(node, rendermtx);
        let Some(anim) = anim else {
            let local = Mat4::from_translation(rodata_pos);
            self.matrices[mtx[0] as usize] = match parent {
                Some(p) => pdmtx::mul(&p, &local),
                None => local,
            };
            return;
        };

        let mut rot1 = Vec3::ZERO;
        let mut translate1 = Vec3::ZERO;
        let mut scale1 = Vec3::ONE;
        let mut sp128 = false;
        if anim.animnum != 0 {
            if let Some(ad) = bank.get(anim.animnum) {
                sp128 = ad.flags & ANIMFLAG_ABSOLUTETRANSLATION != 0 && is_root;
                let (r, t, s) = ad.rot_translate_scale(animpart, anim.framea);
                rot1 = r;
                translate1 = t;
                scale1 = s;
                if anim.frac != 0.0 {
                    let (r2, t2, _) = ad.rot_translate_scale(animpart, anim.frameb);
                    rot1 = pdmtx::tween_rot(rot1, r2, anim.frac);
                    if sp128 {
                        translate1 += (t2 - translate1) * anim.frac;
                    }
                }
            }
        }

        if anim.fracmerge != 0.0 {
            // Merge: slerp from the old animation's rotation (`quaternion_slerp`,
            // fracmerge 1 = all old). Translations are not merged.
            let mut rot3 = Vec3::ZERO;
            if let Some(ad2) = bank.get(anim.animnum2) {
                let (r3, _, _) = ad2.rot_translate_scale(animpart, anim.frame2a);
                rot3 = r3;
                if anim.frac2 != 0.0 {
                    let (r4, _, _) = ad2.rot_translate_scale(animpart, anim.frame2b);
                    rot3 = pdmtx::tween_rot(rot3, r4, anim.frac2);
                }
            }
            let q1 = euler_quat(rot1);
            let mut q3 = euler_quat(rot3);
            if q1.dot(q3) < 0.0 {
                q3 = -q3;
            }
            let q = q1.slerp(q3, anim.fracmerge);
            let pos = if translate1 != Vec3::ZERO {
                let mut t = translate1 * anim.animscale;
                if !is_root {
                    t += rodata_pos;
                }
                t
            } else if !is_root {
                rodata_pos
            } else {
                translate1
            };
            let mut local = Mat4::from_rotation_translation(q, pos);
            apply_scale(&mut local, scale1);
            self.place_joint(mtx[0] as usize, parent, local, joint_fn);
            return;
        }

        let pos = if sp128 {
            // bg_get_stage_translation_thing() — 1.0 outside cutscene stages.
            translate1
        } else if translate1 != Vec3::ZERO {
            let mut t = translate1 * anim.animscale;
            if !is_root {
                t += rodata_pos;
            }
            t
        } else if !is_root {
            rodata_pos
        } else {
            translate1
        };
        // model_position_joint_using_vec_rot (model.c:834)
        let mut local = pdmtx::load_rotation_translation(pos, rot1);
        if sp128 && self.scale != 1.0 {
            pdmtx::scale3(&mut local, self.scale);
        }
        apply_scale(&mut local, scale1);
        self.place_joint(mtx[0] as usize, parent, local, joint_fn);
    }

    fn place_joint(&mut self, slot: usize, parent: Option<Mat4>, local: Mat4, joint_fn: &mut Option<JointFn>) {
        match parent {
            Some(p) => {
                let mut m = pdmtx::mul(&p, &local);
                if let Some(f) = joint_fn.as_mut() {
                    f(slot, &mut m);
                }
                self.matrices[slot] = m;
            }
            None => self.matrices[slot] = local,
        }
    }

    /// `model_update_chr_node_mtx` (`model.c:726`) — the head-bob model's root.
    fn update_chr_node(&mut self, node: usize, animpart: usize, slot: usize, rendermtx: &Mat4, anim: &Anim, bank: &AnimBank) {
        let parent = self.parent_mtx(node, rendermtx);
        let mut rot1 = Vec3::ZERO;
        if let Some(ad) = bank.get(anim.animnum) {
            rot1 = ad.rot_translate_scale(animpart, anim.framea).0;
            if anim.frac != 0.0 {
                let r2 = ad.rot_translate_scale(animpart, anim.frameb).0;
                rot1 = pdmtx::tween_rot(rot1, r2, anim.frac);
            }
        }
        let rotm = if anim.fracmerge != 0.0 {
            let mut rot3 = Vec3::ZERO;
            if let Some(ad2) = bank.get(anim.animnum2) {
                rot3 = ad2.rot_translate_scale(animpart, anim.frame2a).0;
                if anim.frac2 != 0.0 {
                    let r4 = ad2.rot_translate_scale(animpart, anim.frame2b).0;
                    rot3 = pdmtx::tween_rot(rot3, r4, anim.frac2);
                }
            }
            let q1 = euler_quat(rot1);
            let mut q3 = euler_quat(rot3);
            if q1.dot(q3) < 0.0 {
                q3 = -q3;
            }
            Mat4::from_quat(q1.slerp(q3, anim.fracmerge))
        } else {
            pdmtx::load_rotation(rot1)
        };
        let ci = &self.chrinfo;
        let mut yrot = ci.yrot;
        if ci.unk18 != 0.0 {
            yrot = pdmtx::tween_rot_axis(yrot, ci.unk1c, ci.unk18);
        }
        let mut sp198 = pdmtx::load_y_rotation(yrot);
        pdmtx::set_translation(&mut sp198, ci.pos);
        let mut sp158 = pdmtx::mul(&sp198, &rotm);
        if self.scale != 1.0 {
            // mtx00015f4c: the 3x3 only
            sp158.x_axis *= self.scale;
            sp158.y_axis *= self.scale;
            sp158.z_axis *= self.scale;
            sp158.x_axis.w = 0.0;
            sp158.y_axis.w = 0.0;
            sp158.z_axis.w = 0.0;
        }
        self.matrices[slot] = match parent {
            Some(p) => pdmtx::mul(&p, &sp158),
            None => sp158,
        };
    }
}

/// `quaternion0f096ca0`: euler (PD order) → quaternion.
pub fn euler_quat(rot: Vec3) -> Quat {
    Quat::from_euler(glam::EulerRot::ZYX, rot.z, rot.y, rot.x)
}

fn apply_scale(m: &mut Mat4, s: Vec3) {
    if s.x != 1.0 {
        pdmtx::scale_col0(m, s.x);
    }
    if s.y != 1.0 {
        pdmtx::scale_col1(m, s.y);
    }
    if s.z != 1.0 {
        pdmtx::scale_col2(m, s.z);
    }
}

/// The head-bob model, `g_PlayerModeldef` (`modeldata/player.c`): a CHRINFO root
/// (anim part 0, matrix 1), the hips (part 1, matrix 2) and the head (part 2,
/// matrix 0) — `bondheadmatrices[0]` is the head, the camera's bob source.
pub fn player_head_modeldef() -> ModelDef {
    let nodes = vec![
        Node {
            kind: NodeKind::ChrInfo { animpart: 0, mtx: 1 },
            parent: None,
            partnum: None,
            cull_exit: None,
            batches: vec![],
        },
        Node {
            kind: NodeKind::Position {
                pos: Vec3::new(1.177_982, 41.144_371, 0.0),
                animpart: 1,
                mtx: [2, -1, -1],
                flags: 0,
            },
            parent: Some(0),
            partnum: Some(1),
            cull_exit: None,
            batches: vec![],
        },
        Node {
            kind: NodeKind::Position {
                pos: Vec3::new(-2.576_027, 480.429_02, 0.0),
                animpart: 2,
                mtx: [0, -1, -1],
                flags: 0,
            },
            parent: Some(1),
            partnum: Some(2),
            cull_exit: None,
            batches: vec![],
        },
    ];
    ModelDef {
        name: "g_PlayerModeldef".into(),
        nodes,
        parts: HashMap::new(),
        nummatrices: 3,
        materials: vec![],
        batches: vec![],
        file: None,
        bbox: None,
        tex_dir: None,
    }
}
