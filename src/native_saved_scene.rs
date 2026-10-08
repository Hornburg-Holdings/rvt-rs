//! Current native saved-graphics extraction with explicit shared-symbol closure.
use crate::{
    RevitFile,
    native_document::{self, Options, Record, Summary},
    native_saved_mesh::{self, GraphicsMeshes},
};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Serialize)]
pub struct SavedElement {
    pub id: u64,
    pub identity: crate::native_index::Identity,
    pub source: serde_json::Value,
    pub referenced_graph_sources: BTreeMap<u64, serde_json::Value>,
    pub status: String,
    pub meshes: GraphicsMeshes,
    pub render_materials: BTreeMap<usize, crate::native_saved_materials::RenderMaterial>,
    pub unresolved_material_primitives: Vec<usize>,
}
#[derive(Debug, Serialize)]
pub struct SavedScene {
    pub source_sha256: Option<String>,
    pub format: &'static str,
    pub units: &'static str,
    pub coordinate_frame: &'static str,
    pub complete_document_geometry: bool,
    pub detail_level: i64,
    pub graphics_profile: &'static str,
    pub summaries: Vec<Summary>,
    pub elements: Vec<SavedElement>,
}
fn saved_element_status(meshes: &GraphicsMeshes) -> &'static str {
    // Selection exclusions are intentional profile decisions and stay visible
    // as counters without tainting an otherwise valid mesh. An unbounded face,
    // or a decoder diagnostic, means the owner was not fully tessellated.
    if meshes.diagnostics.is_empty() && meshes.unbounded_faces == 0 {
        "decoded_saved_graphics"
    } else {
        "partial_saved_graphics"
    }
}
pub fn extract(file: &mut RevitFile, options: &Options) -> Result<SavedScene> {
    extract_at_detail(file, options, 3)
}
pub fn extract_at_detail(
    file: &mut RevitFile,
    options: &Options,
    detail_level: i64,
) -> Result<SavedScene> {
    ensure!(
        (1..=3).contains(&detail_level),
        "detail level must be 1, 2 or 3"
    );
    let GraphicsRecords {
        records,
        roots,
        mut summaries,
    } = collect_graphics_records(file, options, true)?;
    let mut resolver = crate::native_saved_materials::Resolver::default_viewport_profile();
    let mut material_options = options.clone();
    material_options.channels = BTreeSet::from([102]);
    material_options.selected_ids.clear();
    summaries.push(native_document::extract(file, &material_options, |r| {
        resolver.ingest(&r)
    })?);
    let mut elements = Vec::new();
    for id in roots {
        let r = &records[&id];
        let meshes = meshes_for_record(r, &records, detail_level);
        let refs: BTreeSet<_> = meshes
            .primitives
            .iter()
            .filter_map(|p| p.source_owner_id)
            .collect();
        let referenced_graph_sources = refs
            .into_iter()
            .map(|id| Ok((id, serde_json::to_value(&records[&id].source)?)))
            .collect::<Result<_>>()?;
        let mut render_materials = BTreeMap::new();
        let mut unresolved_material_primitives = Vec::new();
        for (index, p) in meshes.primitives.iter().enumerate() {
            match resolver.resolve(
                id,
                p.source_owner_id,
                p.face_tag,
                p.render_style_id,
                p.material_id,
            ) {
                Some(m) => {
                    render_materials.insert(index, m);
                }
                None => unresolved_material_primitives.push(index),
            }
        }
        elements.push(SavedElement {
            render_materials,
            unresolved_material_primitives,
            id,
            identity: r.identity.clone(),
            source: serde_json::to_value(&r.source)?,
            referenced_graph_sources,
            status: saved_element_status(&meshes).into(),
            meshes,
        });
    }
    Ok(SavedScene {
        source_sha256: None,
        format: "rvt-native-saved-scene-v1",
        units: "feet",
        coordinate_frame: "document Z-up",
        complete_document_geometry: false,
        detail_level,
        graphics_profile: "default 3D viewport (qualified supported subset)",
        summaries,
        elements,
    })
}

/// Stage compact meshes for consumers that do not use native metadata or
/// render materials. Root graphs are decoded one at a time; only the shared
/// graphics closure is retained. All fallible extraction completes before the
/// first callback. Unsupported graphs and tessellation remain mesh diagnostics.
pub(crate) fn extract_meshes_at_detail(
    file: &mut RevitFile,
    options: &Options,
    detail_level: i64,
    mut emit: impl FnMut(u64, GraphicsMeshes),
) -> Result<Vec<Summary>> {
    ensure!(
        (1..=3).contains(&detail_level),
        "detail level must be 1, 2 or 3"
    );
    let mut opts = options.clone();
    opts.channels = BTreeSet::from([103]);
    let mut roots = BTreeSet::new();
    let mut wanted = BTreeSet::new();
    let first = native_document::extract_graphs(file, &opts, |record| {
        ensure!(
            roots.insert(record.identity.element_id),
            "duplicate current graphics owner"
        );
        collect_graphics_dependencies(&record, &mut wanted);
        // A root's full JSON graph is needed only when its mesh is decoded,
        // after the shared-symbol closure is known.
        Ok(())
    })?;
    let mut summaries = vec![first];
    if roots.is_empty() {
        return Ok(summaries);
    }

    let mut records = BTreeMap::<u64, Record>::new();
    let mut attempted = BTreeSet::<u64>::new();
    for depth in 0..=128 {
        if wanted.is_empty() {
            break;
        }
        ensure!(depth < 128, "shared graphics closure depth budget");
        attempted.extend(&wanted);
        opts.selected_ids = std::mem::take(&mut wanted);
        // A dependency request names exact current IDs, regardless of how
        // the caller selected its root records.
        opts.selected_classes.clear();
        summaries.push(native_document::extract_graphs(file, &opts, |record| {
            collect_graphics_dependencies(&record, &mut wanted);
            ensure!(
                records.insert(record.identity.element_id, record).is_none(),
                "duplicate shared graphics owner"
            );
            Ok(())
        })?);
        wanted.retain(|id| !attempted.contains(id));
    }

    // Roots referenced by another root are deliberately present in `records`.
    // The owning document is immutable, so replay selects the same current
    // records while keeping each other root graph only through tessellation.
    opts.selected_ids = roots.clone();
    opts.selected_classes.clear();
    let mut meshes = BTreeMap::new();
    summaries.push(native_document::extract_graphs(file, &opts, |record| {
        let id = record.identity.element_id;
        ensure!(roots.contains(&id), "unexpected replayed graphics owner");
        ensure!(
            !meshes.contains_key(&id),
            "duplicate replayed graphics owner"
        );
        meshes.insert(id, meshes_for_record(&record, &records, detail_level));
        Ok(())
    })?);
    ensure!(
        meshes.len() == roots.len(),
        "current graphics owners disappeared during replay"
    );
    // The compact staged meshes own their values. Release shared graphs before
    // the consumer starts attaching geometry to its existing model.
    drop(records);
    for (id, mesh) in meshes {
        emit(id, mesh);
    }
    Ok(summaries)
}

fn collect_graphics_dependencies(record: &Record, ids: &mut BTreeSet<u64>) {
    if let Some(graph) = &record.graph {
        for object in &graph.objects {
            if object.class_name == "InstanceInfo" {
                if let Some(id) = crate::native_metadata::identifier(&object.fields["m_symbolId"])
                    .ok()
                    .and_then(|id| u64::try_from(id).ok())
                    .filter(|id| *id > 0)
                {
                    ids.insert(id);
                }
            }
        }
    }
}

struct GraphicsRecords {
    records: BTreeMap<u64, Record>,
    roots: BTreeSet<u64>,
    summaries: Vec<Summary>,
}

fn extract_records(
    file: &mut RevitFile,
    options: &Options,
    include_metadata: bool,
    emit: impl FnMut(Record) -> Result<()>,
) -> Result<Summary> {
    if include_metadata {
        native_document::extract(file, options, emit)
    } else {
        native_document::extract_graphs(file, options, emit)
    }
}

fn collect_graphics_records(
    file: &mut RevitFile,
    options: &Options,
    include_metadata: bool,
) -> Result<GraphicsRecords> {
    let mut opts = options.clone();
    opts.channels = BTreeSet::from([103]);
    let mut records = BTreeMap::<u64, Record>::new();
    let first = extract_records(file, &opts, include_metadata, |r| {
        ensure!(
            records.insert(r.identity.element_id, r).is_none(),
            "duplicate current graphics owner"
        );
        Ok(())
    })?;
    let roots: BTreeSet<_> = records.keys().copied().collect();
    let mut summaries = vec![first];
    let mut attempted = roots.clone();
    for depth in 0..=128 {
        let mut wanted = BTreeSet::new();
        for r in records.values() {
            if let Some(g) = &r.graph {
                for o in &g.objects {
                    if o.class_name == "InstanceInfo" {
                        if let Some(id) =
                            crate::native_metadata::identifier(&o.fields["m_symbolId"])
                                .ok()
                                .and_then(|id| u64::try_from(id).ok())
                                .filter(|id| *id > 0)
                        {
                            if !attempted.contains(&id) {
                                wanted.insert(id);
                            }
                        }
                    }
                }
            }
        }
        if wanted.is_empty() {
            break;
        }
        ensure!(depth < 128, "shared graphics closure depth budget");
        attempted.extend(&wanted);
        opts.selected_ids = wanted;
        summaries.push(extract_records(file, &opts, include_metadata, |r| {
            ensure!(
                records.insert(r.identity.element_id, r).is_none(),
                "duplicate shared graphics owner"
            );
            Ok(())
        })?);
    }
    Ok(GraphicsRecords {
        records,
        roots,
        summaries,
    })
}

fn meshes_for_record(
    record: &Record,
    records: &BTreeMap<u64, Record>,
    detail_level: i64,
) -> GraphicsMeshes {
    let mut meshes = match &record.graph {
        Some(graph) => native_saved_mesh::graphics_with_resolver_at_detail(
            graph,
            &|id| records.get(&id).and_then(|r| r.graph.as_ref()),
            detail_level,
        ),
        None => GraphicsMeshes::default(),
    };
    if record.graph.is_none() {
        meshes.diagnostics.push(
            record
                .diagnostic
                .clone()
                .unwrap_or_else(|| "graphics graph unavailable".into()),
        );
    }
    meshes
}
