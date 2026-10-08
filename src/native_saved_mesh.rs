//! Tessellation of bounded saved planar and cylindrical BRep faces. No application or vendor runtime.
use crate::native_parameters::ObjectGraph;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceMesh {
    pub face_index: usize,
    pub face_tag: i64,
    pub render_style_id: i64,
    pub vertices: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub normal: [f64; 3],
    #[serde(default)]
    pub normals: Vec<[f64; 3]>,
    #[serde(default)]
    pub analytic_surface: Option<AnalyticCylinder>,
    /// Saved UV samples concatenated in loop order; topology remains in the graph.
    #[serde(default)]
    pub trim_uv: Vec<[f64; 2]>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticCylinder {
    pub center: [f64; 3],
    pub radius: f64,
    pub x_vec: [f64; 3],
    pub y_vec: [f64; 3],
    pub z_vec: [f64; 3],
    pub orient_flag: bool,
    pub u_range: [f64; 2],
    pub v_range: [f64; 2],
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CylinderTessellationProfile {
    pub chord_error: f64,
    pub max_v_edge: f64,
}
impl Default for CylinderTessellationProfile {
    fn default() -> Self {
        Self {
            chord_error: 0.001,
            max_v_edge: 1.0,
        }
    }
}
struct PointerIndex<'a> {
    graph: &'a ObjectGraph,
    edges: BTreeMap<(usize, usize), Vec<usize>>,
    tokens: BTreeMap<u64, Vec<usize>>,
}
impl<'a> PointerIndex<'a> {
    fn new(graph: &'a ObjectGraph) -> Self {
        let mut edges: BTreeMap<_, Vec<_>> = BTreeMap::new();
        let mut tokens: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for (i, e) in graph.edges.iter().enumerate() {
            edges
                .entry((e.source_object_index, e.pointer_offset))
                .or_default()
                .push(i);
        }
        for (i, o) in graph.objects.iter().enumerate() {
            tokens.entry(o.token as u64).or_default().push(i);
        }
        Self {
            graph,
            edges,
            tokens,
        }
    }
    fn resolve(&self, source: usize, v: &Value) -> Result<Option<usize>> {
        let g = self.graph;
        let t = v["pointer_token"]
            .as_u64()
            .context("missing pointer token")?;
        if t == 0 {
            return Ok(None);
        }
        let off = v["offset"].as_u64().context("pointer offset")? as usize;
        let edges = self
            .edges
            .get(&(source, off))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if edges.len() == 1 {
            let e = &g.edges[edges[0]];
            let target = g
                .objects
                .get(e.target_object_index)
                .context("pointer target out of bounds")?;
            ensure!(
                e.pointer_token as u64 == t && target.class_tag == e.target_class_tag,
                "pointer edge metadata mismatch"
            );
            if let Some(tag) = v["class_tag"].as_u64() {
                ensure!(
                    tag == target.class_tag as u64,
                    "pointer target class mismatch"
                )
            }
            return Ok(Some(e.target_object_index));
        }
        ensure!(edges.is_empty(), "ambiguous pointer edge");
        ensure!(t != u32::MAX as u64, "unresolved anonymous pointer");
        let objs = self.tokens.get(&t).map(Vec::as_slice).unwrap_or(&[]);
        ensure!(objs.len() == 1, "unresolved or ambiguous token {t}");
        Ok(Some(objs[0]))
    }
}
pub fn pointer(g: &ObjectGraph, source: usize, v: &Value) -> Result<Option<usize>> {
    let t = v["pointer_token"]
        .as_u64()
        .context("missing pointer token")?;
    if t == 0 {
        return Ok(None);
    }
    let off = v["offset"].as_u64().context("pointer offset")? as usize;
    let edges: Vec<_> = g
        .edges
        .iter()
        .filter(|e| e.source_object_index == source && e.pointer_offset == off)
        .collect();
    if edges.len() == 1 {
        let e = edges[0];
        let target = g
            .objects
            .get(e.target_object_index)
            .context("pointer target out of bounds")?;
        ensure!(
            e.pointer_token as u64 == t && target.class_tag == e.target_class_tag,
            "pointer edge metadata mismatch"
        );
        if let Some(tag) = v["class_tag"].as_u64() {
            ensure!(
                tag == target.class_tag as u64,
                "pointer target class mismatch"
            )
        }
        return Ok(Some(e.target_object_index));
    }
    ensure!(edges.is_empty(), "ambiguous pointer edge");
    ensure!(t != u32::MAX as u64, "unresolved anonymous pointer");
    let objs: Vec<_> = g
        .objects
        .iter()
        .enumerate()
        .filter(|(_, o)| o.token as u64 == t)
        .collect();
    ensure!(objs.len() == 1, "unresolved or ambiguous token {t}");
    Ok(Some(objs[0].0))
}
fn point<const N: usize>(v: &Value) -> Result<[f64; N]> {
    let a = v.as_array().context("point array")?;
    ensure!(a.len() == N, "point dimension");
    let mut p = [0.; N];
    for (i, x) in a.iter().enumerate() {
        p[i] = x.as_f64().context("point number")?;
        ensure!(p[i].is_finite(), "nonfinite point")
    }
    Ok(p)
}
fn close(a: [f64; 2], b: [f64; 2]) -> bool {
    (a[0] - b[0]).hypot(a[1] - b[1]) < 1e-7
}
fn cross(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}
fn area(r: &[[f64; 2]]) -> f64 {
    (0..r.len())
        .map(|i| r[i][0] * r[(i + 1) % r.len()][1] - r[(i + 1) % r.len()][0] * r[i][1])
        .sum::<f64>()
        / 2.
}
fn retain_boundary_points(
    points: &[[f64; 2]],
    boundary_edges: &BTreeSet<(u32, u32)>,
    triangles: &mut Vec<[u32; 3]>,
) -> Result<()> {
    for pi in 0..points.len() {
        if triangles.iter().any(|t| t.contains(&(pi as u32))) {
            continue;
        }
        let p = points[pi];
        let mut split = None;
        for (ti, t) in triangles.iter().enumerate() {
            for edge in 0..3 {
                let a = t[edge] as usize;
                let b = t[(edge + 1) % 3] as usize;
                let c = t[(edge + 2) % 3];
                let edge_key = (
                    t[edge].min(t[(edge + 1) % 3]),
                    t[edge].max(t[(edge + 1) % 3]),
                );
                let boundary_line = boundary_edges.contains(&edge_key)
                    || boundary_edges.iter().any(|(u, v)| {
                        cross(points[*u as usize], points[*v as usize], points[a]).abs() <= 1e-8
                            && cross(points[*u as usize], points[*v as usize], points[b]).abs()
                                <= 1e-8
                            && cross(points[*u as usize], points[*v as usize], p).abs() <= 1e-8
                    });
                if !boundary_line {
                    continue;
                }
                let ab = [points[b][0] - points[a][0], points[b][1] - points[a][1]];
                let ap = [p[0] - points[a][0], p[1] - points[a][1]];
                let length2 = ab[0] * ab[0] + ab[1] * ab[1];
                let dot = ap[0] * ab[0] + ap[1] * ab[1];
                if length2 > 1e-24
                    && cross(points[a], points[b], p).abs() <= 1e-8
                    && dot > 1e-12
                    && dot < length2 - 1e-12
                {
                    split = Some((
                        ti,
                        [t[edge], pi as u32, c],
                        [pi as u32, t[(edge + 1) % 3], c],
                    ));
                    break;
                }
            }
            if split.is_some() {
                break;
            }
        }
        let Some((ti, first, second)) = split else {
            bail!("boundary sample was discarded by triangulation")
        };
        triangles[ti] = first;
        triangles.push(second);
    }
    Ok(())
}

// Refine a particular shared edge in its curved surface's parameter domain.
// The planar side is reconstructed from the same evaluated 3D positions, not
// from interpolation along the old polygon's chords.
fn refined_plane_edge(
    g: &ObjectGraph,
    index: &PointerIndex<'_>,
    face: usize,
    edge_index: usize,
    plane_side: usize,
    original: Vec<[f64; 2]>,
) -> Result<Vec<[f64; 2]>> {
    let edge = &g.objects[edge_index];
    let faces = edge.fields["m_pFace"]
        .as_array()
        .context("edge face pair")?;
    ensure!(faces.len() == 2, "edge face pair length");
    let other_side = 1 - plane_side;
    let Some(other) = index.resolve(edge_index, &faces[other_side])? else {
        return Ok(original);
    };
    let other_surface = index
        .resolve(other, &g.objects[other].fields["m_pSurf"])?
        .context("shared edge surface missing")?;
    let kind = g.objects[other_surface].class_name.as_str();
    if !matches!(kind, "CylSurf" | "SurfRev" | "RuledSurf") {
        return Ok(original);
    }
    type SurfaceEval = Box<dyn Fn([f64; 2]) -> Result<[f64; 3]>>;
    let (u_values, v_values, evaluate): (Vec<f64>, Vec<f64>, SurfaceEval) = if kind == "CylSurf" {
        let profile = CylinderTessellationProfile::default();
        let mesh = cylinder_face(g, other, profile)?;
        let cylinder = mesh
            .analytic_surface
            .context("cylinder analytic metadata missing")?;
        let min: [f64; 2] = std::array::from_fn(|k| {
            mesh.trim_uv
                .iter()
                .map(|p| p[k])
                .fold(f64::INFINITY, f64::min)
        });
        let max: [f64; 2] = std::array::from_fn(|k| {
            mesh.trim_uv
                .iter()
                .map(|p| p[k])
                .fold(f64::NEG_INFINITY, f64::max)
        });
        let step = (4.
            * (profile.chord_error / (2. * cylinder.radius))
                .min(1.)
                .sqrt()
                .asin())
        .min(std::f64::consts::FRAC_PI_2);
        let us = tessellation_axis(min[0], max[0], step, mesh.trim_uv.iter().map(|p| p[0]))?;
        let vs = tessellation_axis(
            min[1],
            max[1],
            profile.max_v_edge,
            mesh.trim_uv.iter().map(|p| p[1]),
        )?;
        let eval = Box::new(move |uv: [f64; 2]| {
            let (sn, cs) = uv[0].sin_cos();
            Ok(std::array::from_fn(|k| {
                cylinder.center[k]
                    + cylinder.radius * (cs * cylinder.x_vec[k] + sn * cylinder.y_vec[k])
                    + uv[1] * cylinder.z_vec[k]
            }))
        });
        (us, vs, eval)
    } else {
        let grid = crate::native_parametric_mesh::face_grid(g, other)?;
        (
            grid.u,
            grid.v,
            Box::new(move |uv: [f64; 2]| grid.surface.evaluate(uv[0], uv[1])),
        )
    };
    let ends = edge.fields["m_firstAndLastEdgePnts"]
        .as_array()
        .context("edge endpoints")?;
    ensure!(ends.len() == 2, "edge endpoint count");
    let mut samples = vec![point::<2>(&ends[0]["uv"][other_side])?];
    for sample in edge.fields["m_interiorEdgePnts"]
        .as_array()
        .context("edge interior samples")?
    {
        samples.push(point::<2>(&sample["uv"][other_side])?);
    }
    samples.push(point::<2>(&ends[1]["uv"][other_side])?);
    let start = samples[0];
    let end = *samples.last().unwrap();
    let axis = if (start[1] - end[1]).abs() < 1e-7 && (start[0] - end[0]).abs() > 1e-7 {
        0
    } else if (start[0] - end[0]).abs() < 1e-7 && (start[1] - end[1]).abs() > 1e-7 {
        1
    } else {
        bail!("shared curved edge is not an isoparametric interval")
    };
    ensure!(
        samples
            .iter()
            .all(|uv| (uv[1 - axis] - start[1 - axis]).abs() < 1e-7),
        "shared curved edge changes its fixed parameter"
    );
    let plane_index = index
        .resolve(face, &g.objects[face].fields["m_pSurf"])?
        .context("plane surface missing")?;
    let plane = &g.objects[plane_index];
    ensure!(
        plane.class_name == "Plane",
        "edge refinement requires a plane"
    );
    let origin = point::<3>(&plane.fields["m_origin"])?;
    let x = point::<3>(&plane.fields["m_xVec"])?;
    let y = point::<3>(&plane.fields["m_yVec"])?;
    let dot = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).map(|(a, b)| a * b).sum::<f64>();
    let xx = dot(x, x);
    let xy = dot(x, y);
    let yy = dot(y, y);
    let det = xx * yy - xy * xy;
    ensure!(
        det.is_finite() && det > 1e-24,
        "singular shared plane basis"
    );
    let project = |uv: [f64; 2]| -> Result<[f64; 2]> {
        let q = evaluate(uv)?;
        let d = std::array::from_fn(|k| q[k] - origin[k]);
        let dx = dot(d, x);
        let dy = dot(d, y);
        let p = [(dx * yy - dy * xy) / det, (dy * xx - dx * xy) / det];
        let residual = (0..3)
            .map(|k| (origin[k] + p[0] * x[k] + p[1] * y[k] - q[k]).powi(2))
            .sum::<f64>();
        ensure!(
            residual <= 1e-14,
            "shared curved edge leaves adjacent plane"
        );
        Ok(p)
    };
    ensure!(
        samples.len() == original.len(),
        "shared edge sample count mismatch"
    );
    for (uv, expected) in samples.iter().zip(&original) {
        ensure!(
            close(project(*uv)?, *expected),
            "shared edge surface samples disagree"
        );
    }
    let knots = if axis == 0 { u_values } else { v_values };
    let lo = start[axis].min(end[axis]);
    let hi = start[axis].max(end[axis]);
    let mut values = knots
        .into_iter()
        .filter(|v| *v > lo + 1e-12 && *v < hi - 1e-12)
        .collect::<Vec<_>>();
    values.insert(0, lo);
    values.push(hi);
    if start[axis] > end[axis] {
        values.reverse();
    }
    values
        .into_iter()
        .map(|t| {
            let mut uv = start;
            uv[axis] = t;
            project(uv)
        })
        .collect()
}
fn inside(p: [f64; 2], r: &[[f64; 2]]) -> bool {
    let mut hit = false;
    for i in 0..r.len() {
        let a = r[i];
        let b = r[(i + 1) % r.len()];
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            hit = !hit
        }
    }
    hit
}
fn ring(
    g: &ObjectGraph,
    index: &PointerIndex<'_>,
    face: usize,
    li: usize,
) -> Result<Vec<[f64; 2]>> {
    ring_impl(g, index, face, li, false, false)
}
fn ring_impl(
    g: &ObjectGraph,
    index: &PointerIndex<'_>,
    face: usize,
    li: usize,
    refine_plane: bool,
    require_isoparametric: bool,
) -> Result<Vec<[f64; 2]>> {
    let l = &g.objects[li];
    ensure!(
        ["EdgeLoop", "EdgeLoopWithChainEnvelopes"].contains(&l.class_name.as_str()),
        "unsupported loop {}",
        l.class_name
    );
    ensure!(l.fields["m_open"] == false, "open face loop");
    ensure!(
        index.resolve(li, &l.fields["m_pFace"])? == Some(face),
        "loop owner mismatch"
    );
    let mut cur = index
        .resolve(li, &l.fields["m_next"])?
        .context("empty loop")?;
    let mut seen = BTreeSet::new();
    let mut segments = Vec::new();
    while cur != li {
        ensure!(seen.insert(cur), "edge chain cycle before loop sentinel");
        let e = &g.objects[cur];
        ensure!(e.class_name == "Edge", "nonedge loop member");
        let faces = e.fields["m_pFace"].as_array().context("edge face pair")?;
        let sides: Vec<_> = faces
            .iter()
            .enumerate()
            .filter_map(|(i, p)| match index.resolve(cur, p) {
                Ok(Some(f)) if f == face => Some(Ok(i)),
                Err(e) => Some(Err(e)),
                _ => None,
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(sides.len() == 1, "ambiguous edge face side");
        let side = sides[0];
        let endpoints = e.fields["m_firstAndLastEdgePnts"]
            .as_array()
            .context("edge endpoints")?;
        ensure!(endpoints.len() == 2, "edge endpoint count");
        let mut pts = vec![point::<2>(&endpoints[0]["uv"][side])?];
        for p in e.fields["m_interiorEdgePnts"]
            .as_array()
            .context("edge interior points")?
        {
            pts.push(point::<2>(&p["uv"][side])?)
        }
        pts.push(point::<2>(&endpoints[1]["uv"][side])?);
        if require_isoparametric {
            ensure!(
                (0..2).any(|axis| { pts.iter().all(|p| (p[axis] - pts[0][axis]).abs() <= 1e-12) }),
                "nonisoparametric cylinder trim unsupported"
            );
        }
        if refine_plane {
            pts = refined_plane_edge(g, index, face, cur, side, pts)?;
        }
        segments.push(pts);
        cur = index
            .resolve(cur, &e.fields["m_next"][side])?
            .context("null next edge")?;
    }
    // Saved circular loops can be represented by two connected arc edges
    // (typically two semicircles). A third edge is not a topology invariant;
    // closure, nonzero area, and connected ordering below remain mandatory.
    ensure!(segments.len() >= 2, "too few boundary edges");
    ensure!(
        segments.iter().all(|s: &Vec<_>| !s.is_empty()),
        "empty boundary edge"
    );
    let mut output = None;
    for reverse in [false, true] {
        let mut out = segments[0].clone();
        if reverse {
            out.reverse()
        };
        let mut valid = true;
        for s in &segments[1..] {
            let last = *out.last().unwrap();
            if close(last, s[0]) {
                out.extend_from_slice(&s[1..])
            } else if close(last, *s.last().unwrap()) {
                out.extend(s.iter().rev().skip(1).copied())
            } else {
                valid = false;
                break;
            }
        }
        if valid && close(out[0], *out.last().unwrap()) {
            out.pop();
            output = Some(out);
            break;
        }
    }
    let mut out = output.context("disconnected UV boundary")?;
    out.dedup_by(|a, b| close(*a, *b));
    ensure!(
        out.len() >= 3 && area(&out).abs() > 1e-12,
        "degenerate boundary"
    );
    Ok(out)
}

fn tessellation_axis(
    min: f64,
    max: f64,
    step: f64,
    knots: impl Iterator<Item = f64>,
) -> Result<Vec<f64>> {
    ensure!(
        min.is_finite() && max.is_finite() && step.is_finite() && step > 0. && max > min,
        "invalid tessellation axis"
    );
    let count = ((max - min) / step).ceil();
    ensure!(
        count.is_finite() && (1. ..=2_000_000.).contains(&count),
        "tessellation axis too large"
    );
    let n = count as usize;
    let mut values = (0..=n)
        .map(|i| min + (max - min) * i as f64 / n as f64)
        .collect::<Vec<_>>();
    values.extend(knots.filter(|v| v.is_finite() && *v > min && *v < max));
    values.sort_by(f64::total_cmp);
    values.dedup_by(|a, b| (*a - *b).abs() <= 1e-12);
    ensure!(values.len() >= 2, "degenerate tessellation axis");
    Ok(values)
}

// Only exact isoparametric intervals are admitted on the additional trim path.
// Canonicalization uses the existing grid's 1e-12 knot precision; arbitrary
// sampled curves cannot establish the boundary between their saved samples.
fn orthogonal_cylinder_loops(loops: &[Vec<[f64; 2]>]) -> Result<Vec<Vec<[f64; 2]>>> {
    let count = loops.iter().map(Vec::len).sum::<usize>();
    ensure!(count <= 4096, "cylinder trim validation budget exceeded");
    let axes: [Vec<f64>; 2] = std::array::from_fn(|k| {
        let mut values = loops.iter().flatten().map(|p| p[k]).collect::<Vec<_>>();
        values.sort_by(f64::total_cmp);
        values.dedup_by(|a, b| (*a - *b).abs() <= 1e-12);
        values
    });
    let mut normalized = loops.to_vec();
    for p in normalized.iter_mut().flatten() {
        for k in 0..2 {
            let i = axes[k].partition_point(|v| *v < p[k]);
            let candidate = [i.checked_sub(1), (i < axes[k].len()).then_some(i)]
                .into_iter()
                .flatten()
                .find(|i| (axes[k][*i] - p[k]).abs() <= 1e-12)
                .context("cylinder trim knot precision mismatch")?;
            p[k] = axes[k][candidate];
        }
    }
    let mut edges = Vec::with_capacity(count);
    for (li, r) in normalized.iter().enumerate() {
        ensure!(area(r).abs() > 1e-12, "degenerate cylinder trim loop");
        for i in 0..r.len() {
            let a = r[i];
            let b = r[(i + 1) % r.len()];
            ensure!(
                (a[0] == b[0]) != (a[1] == b[1]),
                "nonisoparametric cylinder trim unsupported"
            );
            ensure!(
                (a[0] - b[0]).hypot(a[1] - b[1]) > 1e-7,
                "repeated cylinder trim vertex"
            );
            edges.push((li, i, a, b));
        }
    }
    for (ei, &(li, i, a, b)) in edges.iter().enumerate() {
        for &(lj, j, c, d) in &edges[ei + 1..] {
            let adjacent = li == lj
                && ((i + 1) % normalized[li].len() == j || (j + 1) % normalized[li].len() == i);
            let vertical_ab = a[0] == b[0];
            let vertical_cd = c[0] == d[0];
            let between = |v: f64, x: f64, y: f64| v >= x.min(y) && v <= x.max(y);
            let intersects = if vertical_ab == vertical_cd {
                let fixed = usize::from(!vertical_ab);
                let axis = 1 - fixed;
                let lo = a[axis].min(b[axis]).max(c[axis].min(d[axis]));
                let hi = a[axis].max(b[axis]).min(c[axis].max(d[axis]));
                a[fixed] == c[fixed] && if adjacent { lo < hi } else { lo <= hi }
            } else if vertical_ab {
                between(a[0], c[0], d[0]) && between(c[1], a[1], b[1]) && !adjacent
            } else {
                between(c[0], a[0], b[0]) && between(a[1], c[1], d[1]) && !adjacent
            };
            ensure!(!intersects, "intersecting or touching cylinder trim loops");
        }
    }
    Ok(normalized)
}

fn cylinder_row_intervals(loops: &[Vec<[f64; 2]>], v: f64) -> Result<Vec<[f64; 2]>> {
    let mut crossings = Vec::new();
    for r in loops {
        for i in 0..r.len() {
            let a = r[i];
            let b = r[(i + 1) % r.len()];
            if a[0] == b[0] && v > a[1].min(b[1]) && v < a[1].max(b[1]) {
                crossings.push(a[0]);
            }
        }
    }
    crossings.sort_by(f64::total_cmp);
    ensure!(
        crossings.len().is_multiple_of(2) && crossings.windows(2).all(|p| p[1] > p[0]),
        "ambiguous cylinder trim scanline"
    );
    Ok(crossings.chunks_exact(2).map(|p| [p[0], p[1]]).collect())
}
/// Returns no mesh for a face with no recovered trimming boundary.
/// A null trim does not establish whether the saved face is physically empty.
pub fn face(g: &ObjectGraph, fi: usize) -> Result<Option<FaceMesh>> {
    face_indexed(g, &PointerIndex::new(g), fi)
}
fn face_indexed(g: &ObjectGraph, index: &PointerIndex<'_>, fi: usize) -> Result<Option<FaceMesh>> {
    let f = g.objects.get(fi).context("face index")?;
    ensure!(f.class_name == "Face", "not Face");
    let Some(mut li) = index.resolve(fi, &f.fields["m_pFirstLoop"])? else {
        return Ok(None);
    };
    ensure!(
        f.fields["m_faceRegions"]
            .as_array()
            .context("face regions")?
            .is_empty(),
        "face regions need subdivision"
    );
    let pi = index
        .resolve(fi, &f.fields["m_pSurf"])?
        .context("face surface")?;
    let p = &g.objects[pi];
    if p.class_name == "CylSurf" {
        return Ok(Some(cylinder_face(
            g,
            fi,
            CylinderTessellationProfile::default(),
        )?));
    }
    if matches!(p.class_name.as_str(), "SurfRev" | "RuledSurf") {
        return Ok(Some(crate::native_parametric_mesh::face(g, fi)?));
    }
    ensure!(
        p.class_name == "Plane",
        "unsupported surface {}",
        p.class_name
    );
    let origin = point::<3>(&p.fields["m_origin"])?;
    let x = point::<3>(&p.fields["m_xVec"])?;
    let y = point::<3>(&p.fields["m_yVec"])?;
    let orient = p.fields["m_orientFlag"]
        .as_bool()
        .context("plane orientation")?
        ^ (f.fields["m_faceFlags_v9"].as_u64().context("face flags")? & 2 != 0);
    let mut normal = [
        x[1] * y[2] - x[2] * y[1],
        x[2] * y[0] - x[0] * y[2],
        x[0] * y[1] - x[1] * y[0],
    ];
    let norm = normal.iter().map(|v| v * v).sum::<f64>().sqrt();
    ensure!(norm > 1e-12, "singular plane");
    for v in &mut normal {
        *v /= if orient { norm } else { -norm }
    }
    let mut loops = Vec::new();
    let mut seen = BTreeSet::new();
    loop {
        ensure!(seen.insert(li), "loop-list cycle");
        loops.push(ring_impl(g, index, fi, li, true, false)?);
        match index.resolve(li, &g.objects[li].fields["m_nextLoop"])? {
            Some(next) => li = next,
            None => break,
        }
    }
    // A face can contain multiple disconnected outer rings, with nested holes.
    let depths: Vec<_> = loops
        .iter()
        .enumerate()
        .map(|(i, r)| {
            loops
                .iter()
                .enumerate()
                .filter(|(j, s)| *j != i && inside(r[0], s))
                .count()
        })
        .collect();
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    for (i, outer) in loops
        .iter()
        .enumerate()
        .filter(|(i, _)| depths[*i] % 2 == 0)
    {
        let mut pts = outer.clone();
        let mut holes = Vec::new();
        let mut boundary_edges = BTreeSet::new();
        let mut add_boundary_edges = |start: usize, end: usize| {
            for k in start..end {
                let a = k as u32;
                let b = (if k + 1 == end { start } else { k + 1 }) as u32;
                boundary_edges.insert((a.min(b), a.max(b)));
            }
        };
        add_boundary_edges(0, pts.len());
        let mut expected = area(outer).abs();
        for (j, h) in loops.iter().enumerate() {
            if depths[j] == depths[i] + 1 && inside(h[0], outer) {
                holes.push(pts.len() as u32);
                let start = pts.len();
                pts.extend_from_slice(h);
                add_boundary_edges(start, pts.len());
                expected -= area(h).abs()
            }
        }
        let mut ts = Vec::<u32>::new();
        earcut::Earcut::new().earcut(pts.iter().copied(), &holes, &mut ts);
        ensure!(
            !ts.is_empty() && ts.len().is_multiple_of(3),
            "triangulation failed"
        );
        let mut earcut_triangles = ts
            .chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .collect::<Vec<_>>();
        ensure!(
            earcut_triangles
                .iter()
                .flatten()
                .all(|i| (*i as usize) < pts.len()),
            "triangle bounds"
        );
        // Earcut can retain collinear points only in zero-area triangles. Drop
        // those first so boundary restoration does not mistake them for usable
        // incident triangles and subsequently leave a T-junction behind.
        earcut_triangles.retain(|t| {
            cross(pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]).abs() > 1e-12
        });
        retain_boundary_points(&pts, &boundary_edges, &mut earcut_triangles)?;
        let mut sum = 0.;
        let base = vertices.len() as u32;
        for t in &mut earcut_triangles {
            ensure!(
                t.iter().all(|v| (*v as usize) < pts.len()),
                "triangle bounds"
            );
            let a = cross(pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]);
            sum += a.abs() / 2.;
            if a.abs() <= 1e-12 {
                continue;
            }
            if (a > 0.) != orient {
                t.swap(1, 2)
            }
            triangles.push([base + t[0], base + t[1], base + t[2]])
        }
        ensure!(
            (sum - expected).abs() <= 1e-7 * expected.max(1.),
            "triangulated area mismatch: {sum} vs {expected}"
        );
        for uv in pts {
            vertices.push(std::array::from_fn(|k| {
                origin[k] + uv[0] * x[k] + uv[1] * y[k]
            }))
        }
    }
    if triangles.is_empty() {
        bail!("no bounded triangles")
    }
    let vertex_count = vertices.len();
    Ok(Some(FaceMesh {
        face_index: fi,
        face_tag: f.fields["m_GInfo"]["m_tag"].as_i64().unwrap_or(-1),
        render_style_id: crate::native_metadata::identifier(&f.fields["m_renderStyleId"])
            .context("render style id")?,
        vertices,
        triangles,
        normal,
        normals: vec![normal; vertex_count],
        analytic_surface: None,
        trim_uv: Vec::new(),
    }))
}

/// Tessellate one Face whose saved surface is a bounded cylindrical surface.
/// The analytic surface and UV trim are retained alongside the mesh.
pub fn cylinder_face(
    g: &ObjectGraph,
    fi: usize,
    profile: CylinderTessellationProfile,
) -> Result<FaceMesh> {
    ensure!(
        profile.chord_error.is_finite() && profile.chord_error > 0.,
        "invalid cylinder chord error"
    );
    ensure!(
        profile.max_v_edge.is_finite() && profile.max_v_edge > 0.,
        "invalid cylinder V edge"
    );
    let index = PointerIndex::new(g);
    let face = g.objects.get(fi).context("face index")?;
    ensure!(face.class_name == "Face", "not Face");
    ensure!(
        face.fields["m_faceRegions"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "face regions need subdivision"
    );
    let surface_index = index
        .resolve(fi, &face.fields["m_pSurf"])?
        .context("face surface")?;
    let surface = &g.objects[surface_index];
    ensure!(
        surface.class_name == "CylSurf",
        "unsupported surface {}",
        surface.class_name
    );
    let center = point::<3>(&surface.fields["m_center"])?;
    let radius = surface.fields["m_radius"]
        .as_f64()
        .context("cylinder radius")?;
    ensure!(radius.is_finite() && radius > 0., "invalid cylinder radius");
    let x_vec = point::<3>(&surface.fields["m_xVec"])?;
    let y_vec = point::<3>(&surface.fields["m_yVec"])?;
    let z_vec = point::<3>(&surface.fields["m_zVec"])?;
    let dot = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    let norm = |a: [f64; 3]| dot(a, a).sqrt();
    ensure!(
        (norm(x_vec) - 1.).abs() < 1e-7
            && (norm(y_vec) - 1.).abs() < 1e-7
            && (norm(z_vec) - 1.).abs() < 1e-7,
        "cylinder basis not unit"
    );
    ensure!(
        dot(x_vec, y_vec).abs() < 1e-7
            && dot(x_vec, z_vec).abs() < 1e-7
            && dot(y_vec, z_vec).abs() < 1e-7,
        "cylinder basis not orthogonal"
    );
    let cross_xy = [
        x_vec[1] * y_vec[2] - x_vec[2] * y_vec[1],
        x_vec[2] * y_vec[0] - x_vec[0] * y_vec[2],
        x_vec[0] * y_vec[1] - x_vec[1] * y_vec[0],
    ];
    let handedness = dot(cross_xy, z_vec);
    ensure!(
        handedness.abs() > 1. - 1e-7,
        "cylinder basis determinant not unit"
    );
    let envelope = surface.fields["m_Envelope"]["m_corners"]
        .as_array()
        .context("cylinder envelope")?;
    ensure!(envelope.len() == 2, "cylinder envelope bounds");
    let u_range = [
        envelope[0][0].as_f64().context("u min")?,
        envelope[1][0].as_f64().context("u max")?,
    ];
    let v_range = [
        envelope[0][1].as_f64().context("v min")?,
        envelope[1][1].as_f64().context("v max")?,
    ];
    ensure!(
        u_range.iter().chain(v_range.iter()).all(|v| v.is_finite()),
        "nonfinite cylinder envelope"
    );
    ensure!(
        u_range[1] > u_range[0]
            && u_range[1] - u_range[0] <= std::f64::consts::TAU + 1e-7
            && v_range[1] > v_range[0],
        "invalid cylinder envelope or seam crossing"
    );
    let loop_index = index
        .resolve(fi, &face.fields["m_pFirstLoop"])?
        .context("cylinder trim loop")?;
    let mut loops = Vec::new();
    let mut seen_loops = BTreeSet::new();
    let mut li = loop_index;
    loop {
        ensure!(seen_loops.insert(li), "cylinder loop-list cycle");
        loops.push(ring(g, &index, fi, li)?);
        match index.resolve(li, &g.objects[li].fields["m_nextLoop"])? {
            Some(next) => li = next,
            None => break,
        }
    }
    let trim_uv = loops.iter().flatten().copied().collect::<Vec<_>>();
    ensure!(!trim_uv.is_empty(), "empty cylinder trim");
    let mut trim_min = [f64::INFINITY; 2];
    let mut trim_max = [f64::NEG_INFINITY; 2];
    for uv in &trim_uv {
        for k in 0..2 {
            trim_min[k] = trim_min[k].min(uv[k]);
            trim_max[k] = trim_max[k].max(uv[k]);
        }
    }
    ensure!(
        (trim_min[0] - trim_max[0]).abs() > 1e-12 && (trim_min[1] - trim_max[1]).abs() > 1e-12,
        "degenerate cylinder trim"
    );
    let tol = 1e-7;
    let rectangular = (|| -> Result<()> {
        ensure!(
            loops.len() == 1 && trim_uv.len() >= 4,
            "nonrectangular cylinder trim"
        );
        let mut covered = [0.; 4];
        let mut perimeter = 0.;
        for (a, b) in trim_uv
            .iter()
            .zip(trim_uv.iter().cycle().skip(1))
            .take(trim_uv.len())
        {
            ensure!(
                a[0] >= u_range[0] - tol
                    && a[0] <= u_range[1] + tol
                    && a[1] >= v_range[0] - tol
                    && a[1] <= v_range[1] + tol
                    && b[0] >= u_range[0] - tol
                    && b[0] <= u_range[1] + tol
                    && b[1] >= v_range[0] - tol
                    && b[1] <= v_range[1] + tol,
                "cylinder trim outside envelope"
            );
            let du = b[0] - a[0];
            let dv = b[1] - a[1];
            let length = du.hypot(dv);
            ensure!(length > tol, "repeated cylinder trim vertex");
            ensure!(
                du.abs() < tol || dv.abs() < tol,
                "nonrectangular cylinder trim"
            );
            let side = if dv.abs() < tol && (a[1] - trim_min[1]).abs() < tol {
                0
            } else if du.abs() < tol && (a[0] - trim_max[0]).abs() < tol {
                1
            } else if dv.abs() < tol && (a[1] - trim_max[1]).abs() < tol {
                2
            } else if du.abs() < tol && (a[0] - trim_min[0]).abs() < tol {
                3
            } else {
                bail!("nonrectangular cylinder trim")
            };
            covered[side] += length;
            perimeter += length;
        }
        let expected_perimeter = 2. * ((trim_max[0] - trim_min[0]) + (trim_max[1] - trim_min[1]));
        ensure!(
            (perimeter - expected_perimeter).abs() <= tol * expected_perimeter.max(1.),
            "incomplete or self-intersecting cylinder trim"
        );
        ensure!(
            (covered[0] - (trim_max[0] - trim_min[0])).abs() <= tol
                && (covered[1] - (trim_max[1] - trim_min[1])).abs() <= tol
                && (covered[2] - (trim_max[0] - trim_min[0])).abs() <= tol
                && (covered[3] - (trim_max[1] - trim_min[1])).abs() <= tol,
            "incomplete cylinder trim perimeter"
        );
        Ok(())
    })()
    .is_ok();
    for uv in &trim_uv {
        ensure!(
            uv[0] >= u_range[0] - tol
                && uv[0] <= u_range[1] + tol
                && uv[1] >= v_range[0] - tol
                && uv[1] <= v_range[1] + tol,
            "cylinder trim outside envelope"
        );
    }
    let orthogonal = if rectangular {
        None
    } else {
        for li in seen_loops {
            ring_impl(g, &index, fi, li, false, true)?;
        }
        Some(orthogonal_cylinder_loops(&loops)?)
    };
    let grid_trim = orthogonal.as_deref().unwrap_or(&loops);
    // Keep a full-turn trim from collapsing to coincident seam vertices when a
    // deliberately coarse profile is supplied. This is a geometric validity
    // bound, independent of the analytic chord-error policy.
    let sagitta_ratio = (profile.chord_error / (2. * radius)).min(1.);
    let angular_step = (4. * sagitta_ratio.sqrt().asin()).min(std::f64::consts::FRAC_PI_2);
    ensure!(
        angular_step.is_finite() && angular_step > 0.,
        "cylinder tessellation precision underflow"
    );
    let u_values = tessellation_axis(
        trim_min[0],
        trim_max[0],
        angular_step,
        grid_trim.iter().flatten().map(|p| p[0]),
    )
    .context("cylinder tessellation grid too large")?;
    let v_values = tessellation_axis(
        trim_min[1],
        trim_max[1],
        profile.max_v_edge,
        grid_trim.iter().flatten().map(|p| p[1]),
    )
    .context("cylinder tessellation grid too large")?;
    const MAX_GRID_VERTICES: usize = 2_000_000;
    ensure!(
        u_values.len() <= MAX_GRID_VERTICES && v_values.len() <= MAX_GRID_VERTICES,
        "cylinder tessellation grid too large"
    );
    let nu = u_values.len() - 1;
    let nv = v_values.len() - 1;
    let grid_vertices = (nu + 1)
        .checked_mul(nv + 1)
        .context("cylinder vertex count overflow")?;
    let grid_triangles = nu
        .checked_mul(nv)
        .and_then(|v| v.checked_mul(2))
        .context("cylinder triangle count overflow")?;
    ensure!(
        grid_vertices <= MAX_GRID_VERTICES && grid_triangles <= MAX_GRID_VERTICES * 2,
        "cylinder tessellation grid too large"
    );
    if orthogonal.is_some() {
        ensure!(
            nv.checked_mul(trim_uv.len())
                .and_then(|work| work.checked_add(grid_triangles / 2))
                .is_some_and(|work| work <= 16_000_000),
            "cylinder trim scanline budget exceeded"
        );
    }
    let orient = surface.fields["m_orientFlag"]
        .as_bool()
        .context("cylinder orientation")?
        ^ (face.fields["m_faceFlags_v9"]
            .as_u64()
            .context("face flags")?
            & 2
            != 0);
    let eval = |u: f64, v: f64| {
        let (s, c) = u.sin_cos();
        [
            center[0] + radius * (c * x_vec[0] + s * y_vec[0]) + v * z_vec[0],
            center[1] + radius * (c * x_vec[1] + s * y_vec[1]) + v * z_vec[1],
            center[2] + radius * (c * x_vec[2] + s * y_vec[2]) + v * z_vec[2],
        ]
    };
    let mut vertices = Vec::new();
    let mut normals = Vec::new();
    for &v in v_values.iter().take(nv + 1) {
        for &u in u_values.iter().take(nu + 1) {
            vertices.push(eval(u, v));
            let (s, c) = u.sin_cos();
            let mut n = [
                c * x_vec[0] + s * y_vec[0],
                c * x_vec[1] + s * y_vec[1],
                c * x_vec[2] + s * y_vec[2],
            ];
            // The normal is the oriented parameter-space Jacobian dP/du ×
            // dP/dv. With an orthonormal reflected frame it is the negative
            // radial vector; the saved UV winding itself remains unchanged.
            if orient != (handedness > 0.) {
                n = [-n[0], -n[1], -n[2]];
            }
            normals.push(n);
        }
    }
    let mut triangles = Vec::new();
    let row = nu + 1;
    let mut filled_uv_area = 0.;
    for j in 0..nv {
        let intervals = if let Some(loops) = &orthogonal {
            cylinder_row_intervals(loops, (v_values[j] + v_values[j + 1]) * 0.5)?
        } else {
            Vec::new()
        };
        let mut interval = 0;
        for i in 0..nu {
            if orthogonal.is_some() {
                let u = (u_values[i] + u_values[i + 1]) * 0.5;
                while interval < intervals.len() && u > intervals[interval][1] {
                    interval += 1;
                }
                if interval == intervals.len() || u < intervals[interval][0] {
                    continue;
                }
            }
            filled_uv_area += (u_values[i + 1] - u_values[i]) * (v_values[j + 1] - v_values[j]);
            let a = (j * row + i) as u32;
            let b = a + 1;
            let c = a + row as u32;
            let d = c + 1;
            if orient {
                triangles.extend([[a, b, d], [a, d, c]]);
            } else {
                triangles.extend([[a, d, b], [a, c, d]]);
            }
        }
    }
    if let Some(loops) = &orthogonal {
        let expected = loops
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let depth = loops
                    .iter()
                    .enumerate()
                    .filter(|(j, other)| *j != i && inside(r[0], other))
                    .count();
                area(r).abs() * if depth % 2 == 0 { 1. } else { -1. }
            })
            .sum::<f64>();
        ensure!(
            expected > 1e-12
                && !triangles.is_empty()
                && (filled_uv_area - expected).abs() <= 1e-7 * expected.max(1.),
            "cylinder trimmed area mismatch"
        );
        // Exterior grid points have no incident filled cell. Keep them out of
        // exported vertex bounds as well as out of the surface triangles.
        let mut remap = vec![u32::MAX; vertices.len()];
        for index in triangles.iter().flatten() {
            remap[*index as usize] = 0;
        }
        let mut used_vertices = Vec::new();
        let mut used_normals = Vec::new();
        for (i, new_index) in remap.iter_mut().enumerate() {
            if *new_index != u32::MAX {
                *new_index = used_vertices.len() as u32;
                used_vertices.push(vertices[i]);
                used_normals.push(normals[i]);
            }
        }
        for triangle in &mut triangles {
            for index in triangle {
                *index = remap[*index as usize];
            }
        }
        vertices = used_vertices;
        normals = used_normals;
    }
    Ok(FaceMesh {
        face_index: fi,
        face_tag: face.fields["m_GInfo"]["m_tag"].as_i64().unwrap_or(-1),
        render_style_id: crate::native_metadata::identifier(&face.fields["m_renderStyleId"])?,
        vertices,
        triangles,
        normal: normals[0],
        normals,
        analytic_surface: Some(AnalyticCylinder {
            center,
            radius,
            x_vec,
            y_vec,
            z_vec,
            orient_flag: orient,
            u_range,
            v_range,
        }),
        trim_uv,
    })
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Primitive {
    pub source_owner_id: Option<u64>,
    pub object_index: usize,
    pub face_tag: i64,
    pub render_style_id: i64,
    pub material_id: Option<i64>,
    pub vertices: Vec<[f64; 3]>,
    pub normals: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GraphicsMeshes {
    pub primitives: Vec<Primitive>,
    pub diagnostics: Vec<String>,
    pub unbounded_faces: usize,
    /// Boundary-empty cutout records recognized by an observed saved profile.
    /// Their source records remain in the native graph; no surface is invented.
    #[serde(default)]
    pub empty_trim_faces: usize,
    pub rejected_filters: usize,
    #[serde(default)]
    pub excluded_visibility_branches: usize,
    #[serde(default)]
    pub excluded_non_surface_branches: usize,
    #[serde(default)]
    pub profile_observations: Vec<String>,
}
/// Decode selected saved graphics. Positions and normals are in the owning
/// document frame; length units remain Revit internal feet.
pub fn graphics(g: &ObjectGraph) -> GraphicsMeshes {
    graphics_with_resolver(g, &|_| None)
}
pub fn graphics_with_resolver<'a>(
    g: &'a ObjectGraph,
    resolver: &dyn Fn(u64) -> Option<&'a ObjectGraph>,
) -> GraphicsMeshes {
    graphics_with_resolver_at_detail(g, resolver, 3)
}
/// Decode the explicit saved3D detail representation (native levels1,2,3).
pub fn graphics_with_resolver_at_detail<'a>(
    g: &'a ObjectGraph,
    resolver: &dyn Fn(u64) -> Option<&'a ObjectGraph>,
    detail_level: i64,
) -> GraphicsMeshes {
    let selection = crate::native_graphics_traversal::select_graphics_with_resolver_at_detail(
        g,
        resolver,
        detail_level,
    );
    let mut out = GraphicsMeshes {
        diagnostics: selection
            .diagnostics
            .iter()
            .map(|d| format!("object {}: {}", d.object_index, d.message))
            .collect(),
        rejected_filters: selection.rejected_filters,
        excluded_visibility_branches: selection.excluded_visibility_branches,
        excluded_non_surface_branches: selection.excluded_non_surface_branches,
        profile_observations: selection.profile_observations,
        ..Default::default()
    };
    let mut pointer_indices = BTreeMap::new();
    for selected in selection.selected {
        let g = match selected.source_owner_id {
            None => g,
            Some(id) => match resolver(id) {
                Some(graph) => graph,
                None => {
                    out.diagnostics
                        .push(format!("resolved graphics owner {id} disappeared"));
                    continue;
                }
            },
        };
        let index = pointer_indices
            .entry(selected.source_owner_id)
            .or_insert_with(|| PointerIndex::new(g));
        let oi = selected.object_index;
        let obj = &g.objects[oi];
        let mut decoded = Vec::new();
        let result = (|| -> Result<()> {
            if obj.class_name == "Geometry" {
                for ptr in obj.fields["m_pFaces"]
                    .as_array()
                    .context("geometry faces")?
                {
                    let fi = index.resolve(oi, ptr)?.context("null geometry face")?;
                    match face_indexed(g, index, fi)? {
                        Some(m) => {
                            let normals = if m.normals.len() == m.vertices.len() {
                                m.normals.clone()
                            } else {
                                vec![m.normal; m.vertices.len()]
                            };
                            decoded.push(Primitive {
                                source_owner_id: selected.source_owner_id,
                                object_index: oi,
                                face_tag: m.face_tag,
                                render_style_id: m.render_style_id,
                                material_id: None,
                                vertices: m.vertices,
                                normals,
                                triangles: m.triangles,
                            })
                        }
                        None => {
                            if crate::native_empty_faces::is_observed_empty_trim_face(g, fi) {
                                out.empty_trim_faces += 1;
                            } else {
                                out.unbounded_faces += 1;
                            }
                        }
                    }
                }
            } else if obj.class_name == "GPolyMesh" {
                let ti = index
                    .resolve(oi, &obj.fields["m_pFacetedTopology"])?
                    .context("faceted topology")?;
                let topology = &g.objects[ti];
                ensure!(
                    topology.class_name == "FacetedTopology0",
                    "unsupported faceted topology"
                );
                let f = &topology.fields;
                let vertices = f["m_pointsArr"]
                    .as_array()
                    .context("mesh points")?
                    .iter()
                    .map(point::<3>)
                    .collect::<Result<Vec<_>>>()?;
                let normals = f["m_normalsArr"]
                    .as_array()
                    .context("mesh normals")?
                    .iter()
                    .map(point::<3>)
                    .collect::<Result<Vec<_>>>()?;
                ensure!(
                    f["m_normalsFlag"].as_u64() == Some(0),
                    "unsupported normal binding"
                );
                let triangles = f["m_facetsArr"]
                    .as_array()
                    .context("mesh facets")?
                    .iter()
                    .map(|v| -> Result<[u32; 3]> {
                        let a = v.as_array().context("facet array")?;
                        ensure!(a.len() == 3, "nontriangle facet");
                        let mut t = [0; 3];
                        for i in 0..3 {
                            t[i] = u32::try_from(a[i].as_u64().context("facet index")?)?;
                            ensure!((t[i] as usize) < vertices.len(), "facet index bounds")
                        }
                        Ok(t)
                    })
                    .collect::<Result<Vec<_>>>()?;
                ensure!(
                    normals.is_empty() || normals.len() == triangles.len(),
                    "unsupported normal cardinality"
                );
                let facet_normals = !normals.is_empty();

                let mut expanded_vertices = Vec::new();
                let mut expanded_normals = Vec::new();
                let mut expanded_triangles = Vec::new();
                for (i, t) in triangles.iter().enumerate() {
                    let base = expanded_vertices.len() as u32;
                    let normal = if facet_normals {
                        normals[i]
                    } else {
                        let a = vertices[t[0] as usize];
                        let b = vertices[t[1] as usize];
                        let c = vertices[t[2] as usize];
                        let u: [f64; 3] = std::array::from_fn(|k| b[k] - a[k]);
                        let v: [f64; 3] = std::array::from_fn(|k| c[k] - a[k]);
                        [
                            u[1] * v[2] - u[2] * v[1],
                            u[2] * v[0] - u[0] * v[2],
                            u[0] * v[1] - u[1] * v[0],
                        ]
                    };
                    for idx in t {
                        expanded_vertices.push(vertices[*idx as usize]);
                        expanded_normals.push(normal);
                    }
                    expanded_triangles.push([base, base + 1, base + 2]);
                }
                let (vertices, normals, triangles) =
                    (expanded_vertices, expanded_normals, expanded_triangles);
                decoded.push(Primitive {
                    source_owner_id: selected.source_owner_id,
                    object_index: oi,
                    face_tag: obj.fields["m_GInfo"]["m_tag"].as_i64().unwrap_or(-1),
                    render_style_id: crate::native_metadata::identifier(
                        &obj.fields["m_interiorGStyleID"],
                    )
                    .context("polymesh style")?,
                    material_id: Some(
                        crate::native_metadata::identifier(&obj.fields["m_materialID"])
                            .context("polymesh material")?,
                    )
                    .filter(|v| *v >= 0),
                    vertices,
                    normals,
                    triangles,
                })
            }
            for mesh in &mut decoded {
                transform(mesh, selected.world_transform)?
            }
            Ok(())
        })();
        match result {
            Ok(()) => out.primitives.extend(decoded),
            Err(e) => out.diagnostics.push(format!("object {oi}: {e:#}")),
        }
    }
    out
}
fn transform(p: &mut Primitive, m: [[f64; 4]; 4]) -> Result<()> {
    let a = [m[0][0], m[1][0], m[2][0]];
    let b = [m[0][1], m[1][1], m[2][1]];
    let c = [m[0][2], m[1][2], m[2][2]];
    let cp = |a: [f64; 3], b: [f64; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let bc = cp(b, c);
    let ca = cp(c, a);
    let ab = cp(a, b);
    let det = (0..3).map(|i| a[i] * bc[i]).sum::<f64>();
    ensure!(
        det.is_finite() && det.abs() > 1e-12,
        "singular graphics transform"
    );
    for v in &mut p.vertices {
        let old = *v;
        *v = std::array::from_fn(|i| m[i][3] + (0..3).map(|j| m[i][j] * old[j]).sum::<f64>());
        ensure!(
            v.iter().all(|x| x.is_finite()),
            "nonfinite transformed vertex"
        )
    }
    for n in &mut p.normals {
        let old = *n;
        *n = std::array::from_fn(|i| (bc[i] * old[0] + ca[i] * old[1] + ab[i] * old[2]) / det);
        let len = n.iter().map(|v| v * v).sum::<f64>().sqrt();
        ensure!(len > 1e-12 && len.is_finite(), "invalid transformed normal");
        for v in n {
            *v /= len
        }
    }
    if det < 0. {
        for t in &mut p.triangles {
            t.swap(1, 2)
        }
    }
    Ok(())
}
