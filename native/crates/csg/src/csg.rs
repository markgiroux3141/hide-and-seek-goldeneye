// BSP-tree CSG implementation.
// Ported from CSG.js by Evan Wallace, adapted for axis-aligned box geometry.

const EPSILON: f32 = 1e-5;

// ─── Vector math helpers ────────────────────────────────────────────

#[inline]
fn dot(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn cross(a: &[f32; 3], b: &[f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn sub(a: &[f32; 3], b: &[f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn lerp_v(a: &[f32; 3], b: &[f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

#[inline]
fn length(v: &[f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

// ─── Plane ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Plane {
    pub normal: [f32; 3],
    pub w: f32,
}

const FRONT: u8 = 1;
const BACK: u8 = 2;

impl Plane {
    pub fn from_points(a: &[f32; 3], b: &[f32; 3], c: &[f32; 3]) -> Option<Self> {
        let ab = sub(b, a);
        let ac = sub(c, a);
        let n = cross(&ab, &ac);
        let len = length(&n);
        if len < EPSILON {
            return None;
        }
        let n = [n[0] / len, n[1] / len, n[2] / len];
        Some(Plane {
            normal: n,
            w: dot(&n, a),
        })
    }

    pub fn flip(&self) -> Plane {
        Plane {
            normal: [-self.normal[0], -self.normal[1], -self.normal[2]],
            w: -self.w,
        }
    }

    /// Split polygon into front/back/coplanar lists.
    /// Coplanar polygons go to coplanar_front or coplanar_back based on normal alignment.
    pub fn split_polygon(
        &self,
        polygon: Polygon,
        coplanar_front: &mut Vec<Polygon>,
        coplanar_back: &mut Vec<Polygon>,
        front: &mut Vec<Polygon>,
        back: &mut Vec<Polygon>,
    ) {
        let mut poly_type: u8 = 0;
        let mut types: Vec<u8> = Vec::with_capacity(polygon.vertices.len());

        for v in &polygon.vertices {
            let t = dot(&self.normal, v) - self.w;
            let vtype = if t < -EPSILON {
                BACK
            } else if t > EPSILON {
                FRONT
            } else {
                0
            };
            poly_type |= vtype;
            types.push(vtype);
        }

        match poly_type {
            0 => {
                // Coplanar
                if dot(&self.normal, &polygon.plane.normal) > 0.0 {
                    coplanar_front.push(polygon);
                } else {
                    coplanar_back.push(polygon);
                }
            }
            FRONT => front.push(polygon),
            BACK => back.push(polygon),
            _ => {
                // Spanning — split the polygon
                let mut f_verts: Vec<[f32; 3]> = Vec::new();
                let mut b_verts: Vec<[f32; 3]> = Vec::new();
                let n = polygon.vertices.len();

                for i in 0..n {
                    let j = (i + 1) % n;
                    let ti = types[i];
                    let tj = types[j];
                    let vi = polygon.vertices[i];
                    let vj = polygon.vertices[j];

                    if ti != BACK {
                        f_verts.push(vi);
                    }
                    if ti != FRONT {
                        b_verts.push(vi);
                    }

                    if (ti | tj) == (FRONT | BACK) {
                        let d = sub(&vj, &vi);
                        let denom = dot(&self.normal, &d);
                        if denom.abs() > EPSILON {
                            let t = (self.w - dot(&self.normal, &vi)) / denom;
                            let v = lerp_v(&vi, &vj, t);
                            f_verts.push(v);
                            b_verts.push(v);
                        }
                    }
                }

                if f_verts.len() >= 3 {
                    if let Some(p) = Polygon::new(f_verts) {
                        front.push(p);
                    }
                }
                if b_verts.len() >= 3 {
                    if let Some(p) = Polygon::new(b_verts) {
                        back.push(p);
                    }
                }
            }
        }
    }
}

// ─── Polygon ────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Polygon {
    pub vertices: Vec<[f32; 3]>,
    pub plane: Plane,
}

impl Polygon {
    pub fn new(vertices: Vec<[f32; 3]>) -> Option<Self> {
        if vertices.len() < 3 {
            return None;
        }
        let plane = Plane::from_points(&vertices[0], &vertices[1], &vertices[2])?;
        Some(Polygon { vertices, plane })
    }

    pub fn flip_mut(&mut self) {
        self.vertices.reverse();
        self.plane = self.plane.flip();
    }
}

// ─── BSP Node ───────────────────────────────────────────────────────

pub struct Node {
    plane: Option<Plane>,
    front: Option<Box<Node>>,
    back: Option<Box<Node>>,
    polygons: Vec<Polygon>,
}

impl Node {
    pub fn new(polygons: Vec<Polygon>) -> Self {
        let mut node = Node {
            plane: None,
            front: None,
            back: None,
            polygons: Vec::new(),
        };
        if !polygons.is_empty() {
            node.build(polygons);
        }
        node
    }

    fn empty() -> Self {
        Node {
            plane: None,
            front: None,
            back: None,
            polygons: Vec::new(),
        }
    }

    /// Flip inside/outside for this BSP tree.
    pub fn invert(&mut self) {
        for poly in &mut self.polygons {
            poly.flip_mut();
        }
        if let Some(ref mut plane) = self.plane {
            *plane = plane.flip();
        }
        if let Some(ref mut f) = self.front {
            f.invert();
        }
        if let Some(ref mut b) = self.back {
            b.invert();
        }
        std::mem::swap(&mut self.front, &mut self.back);
    }

    /// Clip a list of polygons by this BSP tree.
    /// Polygons that end up in "back" with no back child are discarded (inside solid).
    pub fn clip_polygons(&self, polygons: Vec<Polygon>) -> Vec<Polygon> {
        let Some(ref plane) = self.plane else {
            return polygons;
        };

        let mut cf = Vec::new();
        let mut cb = Vec::new();
        let mut f = Vec::new();
        let mut b = Vec::new();

        for poly in polygons {
            plane.split_polygon(poly, &mut cf, &mut cb, &mut f, &mut b);
        }

        // Coplanar front → front list, coplanar back → back list
        f.extend(cf);
        b.extend(cb);

        let front_result = match &self.front {
            Some(node) => node.clip_polygons(f),
            None => f,
        };
        let back_result = match &self.back {
            Some(node) => node.clip_polygons(b),
            None => Vec::new(), // discard: inside solid
        };

        [front_result, back_result].concat()
    }

    /// Clip this node's polygons (and children's) by another BSP tree.
    pub fn clip_to(&mut self, other: &Node) {
        self.polygons = other.clip_polygons(std::mem::take(&mut self.polygons));
        if let Some(ref mut f) = self.front {
            f.clip_to(other);
        }
        if let Some(ref mut b) = self.back {
            b.clip_to(other);
        }
    }

    /// Collect all polygons, consuming the tree.
    pub fn into_all_polygons(self) -> Vec<Polygon> {
        let mut result = self.polygons;
        if let Some(f) = self.front {
            result.extend(f.into_all_polygons());
        }
        if let Some(b) = self.back {
            result.extend(b.into_all_polygons());
        }
        result
    }

    /// Add polygons to this BSP tree.
    pub fn build(&mut self, polygons: Vec<Polygon>) {
        if polygons.is_empty() {
            return;
        }

        if self.plane.is_none() {
            self.plane = Some(polygons[0].plane.clone());
        }

        let mut cf = Vec::new();
        let mut cb = Vec::new();
        let mut f = Vec::new();
        let mut b = Vec::new();
        let plane = self.plane.as_ref().unwrap();

        for poly in polygons {
            plane.split_polygon(poly, &mut cf, &mut cb, &mut f, &mut b);
        }

        // Coplanar polygons stored at this node
        self.polygons.extend(cf);
        self.polygons.extend(cb);

        if !f.is_empty() {
            if self.front.is_none() {
                self.front = Some(Box::new(Node::empty()));
            }
            self.front.as_mut().unwrap().build(f);
        }
        if !b.is_empty() {
            if self.back.is_none() {
                self.back = Some(Box::new(Node::empty()));
            }
            self.back.as_mut().unwrap().build(b);
        }
    }
}

// ─── CSG Operations ─────────────────────────────────────────────────

/// A - B: subtract b's volume from a.
pub fn csg_subtract(a_polys: Vec<Polygon>, b_polys: Vec<Polygon>) -> Vec<Polygon> {
    let mut a = Node::new(a_polys);
    let mut b = Node::new(b_polys);
    a.invert();
    a.clip_to(&b);
    b.clip_to(&a);
    b.invert();
    b.clip_to(&a);
    b.invert();
    a.build(b.into_all_polygons());
    a.invert();
    a.into_all_polygons()
}

/// A + B: union of a and b volumes.
pub fn csg_union(a_polys: Vec<Polygon>, b_polys: Vec<Polygon>) -> Vec<Polygon> {
    let mut a = Node::new(a_polys);
    let mut b = Node::new(b_polys);
    a.clip_to(&b);
    b.clip_to(&a);
    b.invert();
    b.clip_to(&a);
    b.invert();
    a.build(b.into_all_polygons());
    a.into_all_polygons()
}

// ─── Mesh output ────────────────────────────────────────────────────

/// Convert polygon list to indexed triangle mesh via fan triangulation.
pub fn polygons_to_mesh(polygons: &[Polygon]) -> (Vec<f32>, Vec<f32>, Vec<u32>) {
    let mut positions: Vec<f32> = Vec::new();
    let mut normals: Vec<f32> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut vert_count: u32 = 0;

    for poly in polygons {
        if poly.vertices.len() < 3 {
            continue;
        }
        let n = poly.plane.normal;
        let base = vert_count;

        for v in &poly.vertices {
            positions.extend_from_slice(v);
            normals.extend_from_slice(&n);
            vert_count += 1;
        }

        // Fan triangulation (valid for convex polygons)
        for i in 1..(poly.vertices.len() as u32 - 1) {
            indices.push(base);
            indices.push(base + i);
            indices.push(base + i + 1);
        }
    }

    (positions, normals, indices)
}
