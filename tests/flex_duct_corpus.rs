//! Physical flexible-duct identity and geometry provenance on a locally supplied
//! Revit 2024 mechanical project. Set RVT_MECHANICAL_PROJECT to its path.

mod common;

use common::project_fixtures::{self, Project};
use rvt::{
    RevitFile, elem_table,
    ifc::{
        ExportQualityMode, RvtDocExporter,
        entities::{IfcEntity, PropertyValue},
        export_content::RECORD_BBOX_BODY_SOURCE,
    },
    partition_element_records as per,
};
use std::collections::BTreeSet;

#[test]
fn real_flexible_ducts_export_without_rigid_curve_assumptions() {
    let Some(path) = project_fixtures::project(Project::Mechanical) else {
        return;
    };
    let mut file = RevitFile::open(path).expect("open real mechanical project");
    let version = file.basic_file_info().expect("project version").version;
    assert_eq!(
        version, 2024,
        "configured flexible-duct fixture is Revit 2024"
    );
    let declared = elem_table::declared_ids(&elem_table::parse_records(&mut file).expect("index"));
    // The public Autodesk enum gives the adjacent category to centerline
    // helpers. Their records must not become physical duct products.
    let records = per::scan_category_records_multi(
        &mut file,
        version,
        &[per::OST_FLEX_DUCT_CURVES, -2_008_021],
        &declared,
    )
    .expect("scan real flexible-duct and helper records");
    let physical: BTreeSet<_> = records
        .iter()
        .filter(|record| {
            record.builtin_category == per::OST_FLEX_DUCT_CURVES
                && record.is_exported_instance()
                && record.has_volume()
        })
        .map(|record| record.element_id)
        .collect();
    let helpers: BTreeSet<_> = records
        .iter()
        .filter(|record| record.builtin_category == -2_008_021)
        .map(|record| record.element_id)
        .collect();
    assert!(
        !physical.is_empty(),
        "fixture must exercise physical flexible ducts"
    );
    assert!(physical.is_subset(&declared));

    let model = RvtDocExporter
        .export_with_mode_and_limits(&mut file, ExportQualityMode::Geometry, Default::default())
        .expect("production geometry export");
    let mut exported = BTreeSet::new();
    let mut indices = BTreeSet::new();
    for (index, entity) in model.entities.iter().enumerate() {
        let IfcEntity::BuildingElement {
            ifc_type,
            type_guid: Some(tag),
            predefined_type: Some(predefined),
            property_set: Some(set),
            extrusion: Some(body),
            solid_shape,
            ..
        } = entity
        else {
            continue;
        };
        if predefined != "FLEXIBLESEGMENT" {
            continue;
        }
        assert_eq!(ifc_type, "IFCDUCTSEGMENT");
        let id: u32 = tag.parse().expect("numeric flexible-duct identity");
        assert!(exported.insert(id), "one IFC body per flexible-duct id");
        indices.insert(index);
        assert!(physical.contains(&id));
        assert!(
            !helpers.contains(&id),
            "centerline helpers are not physical ducts"
        );
        assert!(body.profile_override.is_none(), "no invented rigid profile");
        assert!(
            solid_shape.is_none(),
            "default body stays its labelled record box"
        );
        assert!(set.properties.iter().any(|property| {
            property.name == "BodySource"
                && matches!(&property.value, PropertyValue::Text(source) if source == RECORD_BBOX_BODY_SOURCE)
        }));
        assert!(set.properties.iter().any(|property| {
            property.name == "ProfileResolved"
                && matches!(property.value, PropertyValue::Boolean(false))
        }));
    }
    assert_eq!(
        exported, physical,
        "all admitted physical flexible ducts export"
    );
    for entity in &model.entities {
        match entity {
            IfcEntity::Port { element, .. } => {
                assert!(
                    !indices.contains(element),
                    "no assumed rigid-curve end ports"
                );
            }
            IfcEntity::ElementPropertySet { element, set } if indices.contains(element) => {
                assert!(
                    !set.properties.iter().any(|property| {
                        matches!(property.name.as_str(), "Length" | "Shape")
                            && matches!(
                                set.name.as_str(),
                                "Pset_FlowSegmentDuctSegment"
                                    | "Pset_FlowSegmentPipeSegment"
                                    | "Pset_DuctSegmentTypeCommon"
                            )
                    }),
                    "no rigid-duct length or profile inferred from a flexible duct's box"
                );
            }
            _ => {}
        }
    }
    let step = rvt::ifc::write_step(&model);
    assert_eq!(
        step.lines()
            .filter(|line| line.contains("=IFCDUCTSEGMENT(") && line.contains(".FLEXIBLESEGMENT."))
            .count(),
        physical.len(),
        "STEP serialization preserves the physical flexible-duct class"
    );
}
