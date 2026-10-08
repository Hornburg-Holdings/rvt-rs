//! Inspect serialized cylindrical trim edges on an authorized real RVT/RFA.
//!
//! Establishes whether rejected cylinder loops consist entirely of saved
//! isoparametric intervals, and independently compares saved cylinder/plane
//! edge samples in model coordinates. It emits counts and owner/face indices,
//! with no authored names, raw bytes or positions. Reproduce against each of
//! the eleven family versions with a file argument and selected owner IDs;
//! unsupported versions/graphs are reported as errors, never inferred.
//!
//! Usage: cargo run --release --example probe_cylinder_trim -- MODEL.rvt OWNER_ID ...

use anyhow::{Context, Result, ensure};
use rvt::{
    RevitFile, native_document, native_graphics_traversal,
    native_parameters::ObjectGraph,
    native_saved_mesh::{self, CylinderTessellationProfile},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn point<const N: usize>(value: &Value) -> Result<[f64; N]> {
    let values = value.as_array().context("saved point array")?;
    ensure!(values.len() == N, "saved point dimension");
    let mut output = [0.; N];
    for (i, value) in values.iter().enumerate() {
        output[i] = value.as_f64().context("saved point scalar")?;
        ensure!(output[i].is_finite(), "nonfinite saved point");
    }
    Ok(output)
}

fn symbol_id(mut value: &Value) -> Option<u64> {
    for _ in 0..128 {
        if let Some(id) = value.as_i64() {
            return u64::try_from(id).ok();
        }
        value = value.get("m_id").or_else(|| value.get("m_id64"))?;
    }
    None
}

fn graphs(file: &mut RevitFile, ids: &BTreeSet<u64>) -> Result<BTreeMap<u64, ObjectGraph>> {
    let mut graphs = BTreeMap::new();
    let mut attempted = BTreeSet::<u64>::new();
    let mut wanted = ids.clone();
    for depth in 0..128 {
        if wanted.is_empty() {
            return Ok(graphs);
        }
        attempted.extend(&wanted);
        let options = native_document::Options {
            selected_ids: wanted,
            channels: BTreeSet::from([103]),
            max_graph_values: 1_000_000,
            max_graph_objects: 1_000_000,
            ..Default::default()
        };
        native_document::extract_graphs(file, &options, |record| {
            let id = record.identity.element_id;
            let graph = record.graph.with_context(|| {
                format!(
                    "owner {id}: incomplete saved graph: {:?}",
                    record.diagnostic
                )
            })?;
            ensure!(
                graphs.insert(id, graph).is_none(),
                "duplicate current owner"
            );
            Ok(())
        })?;
        wanted = graphs
            .values()
            .flat_map(|graph| &graph.objects)
            .filter(|object| object.class_name == "InstanceInfo")
            .filter_map(|object| symbol_id(&object.fields["m_symbolId"]))
            .filter(|id| *id > 0 && !attempted.contains(id))
            .collect();
        ensure!(
            depth < 127 || wanted.is_empty(),
            "shared graph depth budget"
        );
    }
    Ok(graphs)
}

fn inspect_face(graph: &ObjectGraph, face_index: usize, surface_index: usize) -> Result<Value> {
    let surface = &graph.objects[surface_index];
    let center = point::<3>(&surface.fields["m_center"])?;
    let x = point::<3>(&surface.fields["m_xVec"])?;
    let y = point::<3>(&surface.fields["m_yVec"])?;
    let z = point::<3>(&surface.fields["m_zVec"])?;
    let radius = surface.fields["m_radius"].as_f64().context("radius")?;
    let evaluate = |uv: [f64; 2]| -> [f64; 3] {
        let (sin, cos) = uv[0].sin_cos();
        std::array::from_fn(|i| center[i] + radius * (cos * x[i] + sin * y[i]) + uv[1] * z[i])
    };
    let mut loops = Vec::new();
    let mut loop_seen = BTreeSet::new();
    let mut loop_index = native_saved_mesh::pointer(
        graph,
        face_index,
        &graph.objects[face_index].fields["m_pFirstLoop"],
    )?;
    let mut checked_plane_samples = 0;
    let mut max_plane_error = 0f64;
    while let Some(li) = loop_index {
        ensure!(loop_seen.insert(li), "loop cycle");
        let mut current =
            native_saved_mesh::pointer(graph, li, &graph.objects[li].fields["m_next"])?
                .context("empty loop")?;
        let mut edge_seen = BTreeSet::new();
        let mut iso_edges = 0;
        let mut noniso_edges = 0;
        let mut edges = Vec::new();
        while current != li {
            ensure!(edge_seen.insert(current), "edge cycle");
            let edge = &graph.objects[current];
            ensure!(edge.class_name == "Edge", "nonedge loop member");
            let faces = edge.fields["m_pFace"].as_array().context("face pair")?;
            ensure!(faces.len() == 2, "face pair length");
            let mut sides = Vec::new();
            for (side, pointer) in faces.iter().enumerate() {
                if native_saved_mesh::pointer(graph, current, pointer)? == Some(face_index) {
                    sides.push(side);
                }
            }
            ensure!(sides.len() == 1, "ambiguous cylinder side");
            let side = sides[0];
            let endpoints = edge.fields["m_firstAndLastEdgePnts"]
                .as_array()
                .context("edge endpoints")?;
            ensure!(endpoints.len() == 2, "endpoint count");
            let interiors = edge.fields["m_interiorEdgePnts"]
                .as_array()
                .context("edge interiors")?;
            let samples: Vec<_> = std::iter::once(&endpoints[0])
                .chain(interiors)
                .chain(std::iter::once(&endpoints[1]))
                .collect();
            let uv: Vec<_> = samples
                .iter()
                .map(|sample| point::<2>(&sample["uv"][side]))
                .collect::<Result<_>>()?;
            let fixed_parameter_deviation = (0..2)
                .map(|axis| {
                    uv.iter()
                        .map(|p| (p[axis] - uv[0][axis]).abs())
                        .fold(0., f64::max)
                })
                .fold(f64::INFINITY, f64::min);
            let is_iso = fixed_parameter_deviation <= 1e-12;
            if is_iso {
                iso_edges += 1;
            } else {
                noniso_edges += 1;
            }
            edges.push(
                json!({"edge":current, "samples":uv.len(), "isoparametric":is_iso,
                "fixed_parameter_deviation":fixed_parameter_deviation}),
            );
            let other_surface = match native_saved_mesh::pointer(graph, current, &faces[1 - side])?
            {
                Some(other) => native_saved_mesh::pointer(
                    graph,
                    other,
                    &graph.objects[other].fields["m_pSurf"],
                )?,
                None => None,
            };
            if let Some(pi) = other_surface.filter(|pi| graph.objects[*pi].class_name == "Plane") {
                let plane = &graph.objects[pi];
                let o = point::<3>(&plane.fields["m_origin"])?;
                let px = point::<3>(&plane.fields["m_xVec"])?;
                let py = point::<3>(&plane.fields["m_yVec"])?;
                for (sample, uv) in samples.iter().zip(&uv) {
                    let puv = point::<2>(&sample["uv"][1 - side])?;
                    let q = evaluate(*uv);
                    let error = (0..3)
                        .map(|k| (q[k] - o[k] - puv[0] * px[k] - puv[1] * py[k]).powi(2))
                        .sum::<f64>()
                        .sqrt();
                    max_plane_error = max_plane_error.max(error);
                    checked_plane_samples += 1;
                }
            }
            current = native_saved_mesh::pointer(graph, current, &edge.fields["m_next"][side])?
                .context("null next edge")?;
        }
        loops.push(json!({"loop":li, "edges":edges, "isoparametric_edges":iso_edges, "nonisoparametric_edges":noniso_edges}));
        loop_index =
            native_saved_mesh::pointer(graph, li, &graph.objects[li].fields["m_nextLoop"])?;
    }
    let mesh =
        native_saved_mesh::cylinder_face(graph, face_index, CylinderTessellationProfile::default());
    Ok(json!({
        "face":face_index, "loops":loops,
        "checked_adjacent_plane_samples":checked_plane_samples,
        "max_adjacent_plane_error_feet":max_plane_error,
        "mesh": match mesh {Ok(mesh)=>json!({"triangles":mesh.triangles.len()}), Err(error)=>json!({"error":format!("{error:#}")})}
    }))
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .context("usage: probe_cylinder_trim MODEL.rvt OWNER_ID ...")?;
    let ids = args
        .map(|id| {
            id.to_str()
                .context("owner ID UTF-8")?
                .parse::<u64>()
                .context("owner ID integer")
        })
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(!ids.is_empty(), "at least one owner ID is required");
    let mut file = RevitFile::open(path)?;
    let graphs = graphs(&mut file, &ids)?;
    let resolver = |id| graphs.get(&id);
    let mut rows = Vec::new();
    let mut checked_faces = BTreeSet::new();
    for id in &ids {
        let graph = graphs
            .get(id)
            .with_context(|| format!("owner {id} absent"))?;
        let selection = native_graphics_traversal::select_graphics_with_resolver(graph, &resolver);
        for selected in selection.selected {
            let source_id = selected.source_owner_id.unwrap_or(*id);
            let source = &graphs[&source_id];
            let object = &source.objects[selected.object_index];
            if object.class_name != "Geometry" {
                continue;
            }
            for pointer in object.fields["m_pFaces"]
                .as_array()
                .context("geometry faces")?
            {
                let fi = native_saved_mesh::pointer(source, selected.object_index, pointer)?
                    .context("null face")?;
                if !checked_faces.insert((source_id, fi)) {
                    continue;
                }
                let pi =
                    native_saved_mesh::pointer(source, fi, &source.objects[fi].fields["m_pSurf"])?
                        .context("null surface")?;
                if source.objects[pi].class_name != "CylSurf" {
                    continue;
                }
                rows.push(json!({"owner":id,"source_owner":source_id,"inspection":inspect_face(source,fi,pi)?}));
            }
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"owners":ids.len(),"graphs":graphs.len(),"cylinders":rows})
        )?
    );
    Ok(())
}
