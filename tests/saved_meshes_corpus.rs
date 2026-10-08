//! Compare IFC attachment with full native-scene extraction on project files
//! supplied through RVT_STRUCTURAL_PROJECT and RVT_MECHANICAL_PROJECT.
//! Select candidates from each file rather than storing model-specific IDs.

mod common;

use common::project_fixtures;
use rvt::{
    RevitFile,
    ifc::{
        ExportQualityMode, RvtDocExporter,
        entities::{IfcEntity, PropertyValue, SolidShape},
        export_content::RECORD_BBOX_BODY_SOURCE,
        saved_meshes,
    },
    native_document, native_saved_scene,
};
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn attached_meshes_preserve_scene_geometry_and_report_partial_owners() {
    for fixture in project_fixtures::projects() {
        let mut file = RevitFile::open(&fixture.path).expect("open real project");
        let mut result = RvtDocExporter
            .export_with_diagnostics_mode_and_limits(
                &mut file,
                ExportQualityMode::Geometry,
                Default::default(),
            )
            .expect("export real project");
        let candidates: BTreeSet<u64> = result
            .model
            .entities
            .iter()
            .filter_map(|entity| {
                let IfcEntity::BuildingElement {
                    type_guid: Some(tag),
                    property_set: Some(set),
                    ..
                } = entity
                else {
                    return None;
                };
                let bbox = set.properties.iter().any(|p| {
                    p.name == "BodySource"
                        && matches!(&p.value, PropertyValue::Text(s) if s == RECORD_BBOX_BODY_SOURCE)
                });
                let resolved = set.properties.iter().any(|p| {
                    p.name == "ProfileResolved"
                        && matches!(p.value, PropertyValue::Boolean(true))
                });
                (bbox && !resolved).then(|| tag.parse().ok()).flatten()
            })
            .collect();
        let mut selected: BTreeSet<u64> = candidates.iter().copied().take(4).collect();
        // Include two occurrences of a reused type, so shared family graphics
        // can be compared alongside independently stored element geometry.
        let mut by_type = BTreeMap::<u32, Vec<u64>>::new();
        for id in &candidates {
            if let Some(type_id) = u32::try_from(*id)
                .ok()
                .and_then(|id| result.model.element_type_ids.get(&id))
            {
                by_type.entry(*type_id).or_default().push(*id);
            }
        }
        if let Some(ids) = by_type.values().find(|ids| ids.len() > 1) {
            selected.extend(ids.iter().copied().take(2));
        }
        assert!(
            !selected.is_empty(),
            "real project has no bounding-box candidates"
        );
        result.model.entities.retain(|entity| {
            matches!(
                entity,
                IfcEntity::BuildingElement {
                    type_guid: Some(tag),
                    ..
                } if tag.parse::<u64>().is_ok_and(|id| selected.contains(&id))
            )
        });
        // A missing required global stream must leave every existing body
        // unchanged when attachment fails.
        let mut compound = cfb::CompoundFile::open(std::io::Cursor::new(
            std::fs::read(&fixture.path).expect("read real file for refusal check"),
        ))
        .expect("open real CFB container");
        compound
            .remove_stream("Global/DocumentIncrementTable")
            .expect("remove increment table from in-memory copy");
        let mut damaged =
            RevitFile::open_bytes(compound.into_inner().into_inner()).expect("open modified copy");
        let mut unchanged = result.model.clone();
        let before = serde_json::to_vec(&unchanged).expect("snapshot model");
        assert!(saved_meshes::attach(&mut damaged, &mut unchanged).is_err());
        assert_eq!(
            serde_json::to_vec(&unchanged).expect("unchanged model"),
            before
        );
        drop(damaged);
        let scene = native_saved_scene::extract_at_detail(
            &mut file,
            &native_document::Options {
                selected_ids: selected,
                max_graph_values: 1_000_000,
                max_graph_objects: 1_000_000,
                ..Default::default()
            },
            3,
        )
        .expect("full native-scene extraction");
        let expected: BTreeMap<_, _> = scene
            .elements
            .iter()
            .map(|element| (element.id, element))
            .collect();
        let report =
            saved_meshes::attach(&mut file, &mut result.model).expect("geometry-only attachment");
        assert!(
            report.replaced > 0,
            "real project must exercise mesh replacement"
        );
        assert_eq!(report.candidates, result.model.entities.len());
        assert_eq!(report.replaced + report.unreplaced, report.candidates);
        let mut replaced = 0;
        let mut partial = 0;
        let mut total_triangles = 0;
        for entity in &result.model.entities {
            let IfcEntity::BuildingElement {
                type_guid: Some(tag),
                location_feet: Some(origin),
                rotation_radians,
                solid_shape:
                    Some(SolidShape::TriangulatedFaceSet {
                        vertices_feet,
                        triangles,
                    }),
                property_set: Some(set),
                ..
            } = entity
            else {
                continue;
            };
            let id: u64 = tag.parse().expect("numeric element Tag");
            let saved = expected[&id];
            let expected_vertices: Vec<_> = saved
                .meshes
                .primitives
                .iter()
                .flat_map(|p| p.vertices.iter())
                .collect();
            assert_eq!(
                vertices_feet.len(),
                expected_vertices.len(),
                "vertex count for {id}"
            );
            // The attachment's existing document-to-local transform must be
            // identical when the root graph is replayed and released early.
            let (sin_local, cos_local) = (-rotation_radians.unwrap_or(0.0)).sin_cos();
            let expected_local: Vec<[f64; 3]> = expected_vertices
                .iter()
                .map(|world| {
                    let (dx, dy) = (world[0] - origin[0], world[1] - origin[1]);
                    [
                        dx * cos_local - dy * sin_local,
                        dx * sin_local + dy * cos_local,
                        world[2] - origin[2],
                    ]
                })
                .collect();
            assert_eq!(
                serde_json::to_vec(vertices_feet).expect("serialize attached vertices"),
                serde_json::to_vec(&expected_local).expect("serialize full-scene vertices"),
                "exact transformed vertex values and order for {id}",
            );
            assert_eq!(
                triangles.len(),
                saved
                    .meshes
                    .primitives
                    .iter()
                    .map(|p| p.triangles.len())
                    .sum::<usize>(),
                "triangle count for {id}"
            );
            let mut base = 0u32;
            let expected_triangles: Vec<_> = saved
                .meshes
                .primitives
                .iter()
                .flat_map(|primitive| {
                    let offset = base;
                    base += primitive.vertices.len() as u32;
                    primitive
                        .triangles
                        .iter()
                        .map(move |t| (offset + t[0], offset + t[1], offset + t[2]))
                })
                .collect();
            assert_eq!(
                triangles
                    .iter()
                    .map(|t| (t.0, t.1, t.2))
                    .collect::<Vec<_>>(),
                expected_triangles,
                "triangle connectivity and winding for {id}"
            );
            let (sin, cos) = rotation_radians.unwrap_or(0.0).sin_cos();
            for (local, expected) in vertices_feet.iter().zip(expected_vertices) {
                let world = [
                    origin[0] + local[0] * cos - local[1] * sin,
                    origin[1] + local[0] * sin + local[1] * cos,
                    origin[2] + local[2],
                ];
                assert!(
                    world
                        .iter()
                        .zip(expected)
                        .all(|(a, b)| (a - b).abs() < 1e-8),
                    "world vertex changed for {id}"
                );
            }
            let is_partial =
                !saved.meshes.diagnostics.is_empty() || saved.meshes.unbounded_faces > 0;
            let mut expected_issues = saved.meshes.diagnostics.clone();
            if saved.meshes.unbounded_faces > 0 {
                expected_issues.push(format!(
                    "{} saved faces could not be bounded",
                    saved.meshes.unbounded_faces,
                ));
            }
            if is_partial {
                assert_eq!(
                    report.issues.get(&id),
                    Some(&expected_issues),
                    "full-scene and streamed-mesh diagnostics for {id}",
                );
            } else {
                assert!(
                    !report.issues.contains_key(&id),
                    "complete full-scene owner gained streaming diagnostics for {id}",
                );
            }
            assert!(
                set.properties
                    .iter()
                    .any(|p| p.name == "SavedGraphicsPartial"
                        && matches!(p.value, PropertyValue::Boolean(value) if value == is_partial))
            );
            if is_partial {
                assert!(report.issues.contains_key(&id));
                partial += 1;
            }
            total_triangles += triangles.len();
            replaced += 1;
        }
        assert_eq!(report.replaced, replaced);
        assert_eq!(report.triangles, total_triangles);
        assert_eq!(report.partial_replaced, partial);
    }
}
