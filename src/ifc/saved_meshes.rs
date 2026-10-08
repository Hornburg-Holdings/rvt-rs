//! Family-instance bodies from the meshes Revit saves for display (#255).
//!
//! A Revit file keeps the graphics it last drew for each element. rosejn's
//! native reader (#255: `native_saved_scene`, `native_saved_mesh` and the
//! schema, graph and identity code under them) decodes them in document
//! feet. Where rvt-rs has only an element record's bounding box, as for
//! doors, windows and furniture, [`attach`] replaces that box with the
//! element's saved mesh.
//!
//! On RE1's four models (Revit 2025) 207 of the 214 bodies replaced are
//! within 0.01 ft of Revit's export (B77, Measure run 37231768480).
//!
//! On Core Interior the saved meshes coincide with the bodies rvt-rs already
//! draws exactly (walls 351 of 360, columns 256 of 256, slabs 80 of 80), and
//! a door's mesh is its panel and frame, 3.50 × 0.83 × 8.25 ft, where its
//! record box is 3.75 ft deep because it includes the swing: the 2.92 ft
//! difference #227 measured against Revit's own export.
//!
//! Reading saved graphics is slow on large files, so it is opt-in
//! (`rvt-ifc --saved-meshes`, `rvt-gltf --saved-meshes`).

use super::IfcModel;
use super::entities::{BrepTriangle, IfcEntity, Property, PropertyValue, SolidShape};
use super::export_content::RECORD_BBOX_BODY_SOURCE;
use std::collections::{BTreeMap, BTreeSet};

/// `BodySource` of a body drawn from the element's saved graphics.
pub const SAVED_MESH_BODY_SOURCE: &str = "saved_graphics_mesh";

/// What [`attach`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SavedMeshReport {
    /// Elements whose body was the record's bounding box.
    pub candidates: usize,
    /// Of those, elements now drawn from their saved mesh.
    pub replaced: usize,
    /// Triangles written in all.
    pub triangles: usize,
    /// Replaced bodies whose saved-graphics traversal or tessellation was partial.
    /// A readable triangle mesh does not establish complete element geometry.
    pub partial_replaced: usize,
    /// Candidate bodies left unchanged, including ambiguous duplicate Tags.
    pub unreplaced: usize,
    /// Element IDs with missing, ambiguous or partially decoded saved graphics.
    pub issues: BTreeMap<u64, Vec<String>>,
}

/// Replace each bounding-box body with the element's saved mesh, where the
/// file holds one.
pub fn attach(rf: &mut crate::RevitFile, model: &mut IfcModel) -> anyhow::Result<SavedMeshReport> {
    let mut report = SavedMeshReport::default();
    let mut by_id: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    // A stair or curtain wall is drawn by its parts; its own saved graphics
    // would draw it twice.
    let wholes: BTreeSet<usize> = model
        .entities
        .iter()
        .filter_map(|entity| match entity {
            IfcEntity::Aggregate { whole, .. } => Some(*whole),
            _ => None,
        })
        .collect();
    for (index, entity) in model.entities.iter().enumerate() {
        if wholes.contains(&index) {
            continue;
        }
        let IfcEntity::BuildingElement {
            type_guid: Some(tag),
            property_set: Some(set),
            location_feet: Some(_),
            ..
        } = entity
        else {
            continue;
        };
        let text = |name: &str| {
            set.properties.iter().find_map(|p| match &p.value {
                PropertyValue::Text(t) if p.name == name => Some(t.as_str()),
                _ => None,
            })
        };
        let profile_resolved = set.properties.iter().any(|p| {
            p.name == "ProfileResolved" && matches!(p.value, PropertyValue::Boolean(true))
        });
        if text("BodySource") != Some(RECORD_BBOX_BODY_SOURCE) || profile_resolved {
            continue;
        }
        let Ok(id) = tag.parse::<u64>() else {
            continue;
        };
        report.candidates += 1;
        by_id.entry(id).or_default().push(index);
    }
    if by_id.is_empty() {
        return Ok(report);
    }
    let options = crate::native_document::Options {
        selected_ids: by_id.keys().copied().collect::<BTreeSet<_>>(),
        max_graph_values: 1_000_000,
        max_graph_objects: 1_000_000,
        ..Default::default()
    };
    let mut seen = BTreeSet::new();
    crate::native_saved_scene::extract_meshes_at_detail(rf, &options, 3, |id, meshes| {
        let Some(indices) = by_id.get(&id) else {
            return;
        };
        seen.insert(id);
        // A Tag carried by several entities (a slab's pieces) is ambiguous:
        // leave them as they are.
        let [index] = indices.as_slice() else {
            report
                .issues
                .insert(id, vec!["Element Tag identifies multiple bodies".into()]);
            return;
        };
        let mut issues = meshes.diagnostics;
        if meshes.unbounded_faces > 0 {
            issues.push(format!(
                "{} saved faces could not be bounded",
                meshes.unbounded_faces
            ));
        }
        let partial = !issues.is_empty();
        let IfcEntity::BuildingElement {
            location_feet: Some(origin),
            rotation_radians,
            solid_shape,
            property_set: Some(set),
            ..
        } = &mut model.entities[*index]
        else {
            return;
        };
        let (sin, cos) = (-rotation_radians.unwrap_or(0.0)).sin_cos();
        let mut vertices = Vec::new();
        let mut triangles = Vec::new();
        for primitive in meshes.primitives {
            let base = vertices.len() as u32;
            vertices.extend(primitive.vertices.iter().map(|p| {
                let (dx, dy) = (p[0] - origin[0], p[1] - origin[1]);
                [dx * cos - dy * sin, dx * sin + dy * cos, p[2] - origin[2]]
            }));
            triangles.extend(
                primitive
                    .triangles
                    .iter()
                    .filter(|t| t.iter().all(|&v| (v as usize) < primitive.vertices.len()))
                    .map(|t| BrepTriangle(base + t[0], base + t[1], base + t[2])),
            );
        }
        if triangles.is_empty() {
            issues.push("No saved surface triangles were recovered".into());
            report.issues.insert(id, issues);
            return;
        }
        report.replaced += 1;
        if partial {
            report.partial_replaced += 1;
            report.issues.insert(id, issues);
        }
        report.triangles += triangles.len();
        *solid_shape = Some(SolidShape::TriangulatedFaceSet {
            vertices_feet: vertices,
            triangles,
        });
        for property in &mut set.properties {
            if property.name == "BodySource" {
                property.value = PropertyValue::Text(SAVED_MESH_BODY_SOURCE.into());
            }
        }
        set.properties.push(Property {
            name: "SavedGraphicsPartial".into(),
            value: PropertyValue::Boolean(partial),
        });
    })?;
    for &id in by_id.keys().filter(|id| !seen.contains(id)) {
        report.issues.insert(
            id,
            vec!["No current saved-graphics record was recovered".into()],
        );
    }
    report.unreplaced = report.candidates - report.replaced;
    Ok(report)
}
