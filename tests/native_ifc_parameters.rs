//! Saved parameters survive native RVT -> IFC occurrence/type export.
//! Compare current native owner records and saved definitions on local files
//! supplied through RVT_STRUCTURAL_PROJECT and RVT_MECHANICAL_PROJECT.

mod common;

use rvt::{
    RevitFile,
    ifc::{
        ExportQualityMode, RvtDocExporter,
        entities::{IfcEntity, PropertySet, PropertyValue},
    },
    native_document,
    native_parameters::Parameter,
};
use std::collections::{BTreeMap, BTreeSet};

fn parameters_for_owner(record: native_document::Record) -> Vec<Parameter> {
    let owner_id = i64::try_from(record.identity.element_id).expect("current native owner ID");
    let owner = record.graph.as_ref().and_then(|graph| {
        let root = graph.objects.first()?;
        let (field, instance) = match root.class_name.as_str() {
            "FamilyInstance" => ("m_pInstParams", true),
            "FamilySymbol" => ("m_pParams", false),
            _ => return None,
        };
        let offset = root.fields[field]["offset"].as_u64()?;
        let target = graph
            .edges
            .iter()
            .find(|edge| edge.source_object_index == 0 && edge.pointer_offset as u64 == offset)?
            .target_object_index;
        assert_eq!(
            graph.objects[target].class_name, "FamilyParams",
            "actual owner pointer selects its family parameter container"
        );
        Some((target, instance))
    });
    let Some(metadata) = record.saved_metadata else {
        return Vec::new();
    };
    let mut parameters: Vec<_> = metadata
        .parameter_sets
        .into_iter()
        .flat_map(|set| set.parameters)
        .collect();
    parameters.extend(
        metadata
            .resolved_family_parameters
            .into_iter()
            .filter(|parameter| {
                parameter.has_value
                    && owner.is_none_or(|(_, instance)| instance || !parameter.stored_instance_flag)
            })
            .map(|parameter| Parameter {
                serialized_parameter_id: parameter.parameter_id,
                storage_type: parameter.storage_type,
                raw_value: parameter.raw_value,
            }),
    );
    parameters.extend(
        metadata
            .raw_field_parameters
            .into_iter()
            .filter(|parameter| {
                parameter.projection_rule == "direct_saved_field"
                    && parameter.source_element_id.is_none_or(|id| id == owner_id)
            })
            .map(|parameter| Parameter {
                serialized_parameter_id: parameter.parameter_id,
                storage_type: parameter.storage_type,
                raw_value: parameter.raw_value,
            }),
    );
    if let Some((object, instance)) = owner {
        parameters.extend(
            metadata
                .family_parameter_slots
                .into_iter()
                .filter(|slot| {
                    slot.source_object == object
                        && (instance || !slot.stored_instance_flag)
                        && matches!(
                            slot.parameter_id,
                            -1_001_203 | -1_001_405 | -1_010_103 | -1_001_206
                        )
                })
                .map(|slot| Parameter {
                    serialized_parameter_id: slot.parameter_id,
                    storage_type: "String".into(),
                    raw_value: slot.string_slot,
                }),
        );
    }
    parameters
}

fn unambiguous(parameters: &[Parameter], id: i64) -> Option<&Parameter> {
    let mut matches = parameters
        .iter()
        .filter(|parameter| parameter.serialized_parameter_id == id);
    let first = matches.next()?;
    matches
        .all(|other| other.storage_type == first.storage_type && other.raw_value == first.raw_value)
        .then_some(first)
}

fn property<'a>(sets: &[&'a PropertySet], name: &str) -> Option<&'a PropertyValue> {
    sets.iter()
        .flat_map(|set| &set.properties)
        .find(|property| property.name == name)
        .map(|property| &property.value)
}

fn check_dimensions(
    source: &[Parameter],
    sets: &[&PropertySet],
    definitions: &BTreeMap<i64, rvt::native_parameter_definitions::Definition>,
) -> usize {
    let mut checked = 0;
    for parameter in source {
        if unambiguous(source, parameter.serialized_parameter_id).is_none() {
            continue;
        }
        let Some(definition) = definitions.get(&parameter.serialized_parameter_id) else {
            continue;
        };
        let Some(spec) = definition
            .spec_type_id
            .as_deref()
            .and_then(|spec| spec.strip_prefix("autodesk.spec.aec:"))
        else {
            continue;
        };
        let spec = spec.split('-').next().unwrap_or(spec);
        if !matches!(spec, "length" | "area" | "volume") || parameter.storage_type != "Double" {
            continue;
        }
        let Some(saved) = parameter
            .raw_value
            .as_f64()
            .filter(|value| value.is_finite())
        else {
            continue;
        };
        let name = if definition.caption.is_empty() {
            format!("RevitParameter:{}", parameter.serialized_parameter_id)
        } else {
            definition.caption.clone()
        };
        let qualified = format!("{name} [{}]", parameter.serialized_parameter_id);
        let value = property(sets, &name).or_else(|| property(sets, &qualified));
        let (typed, factor) = match (spec, value) {
            ("length", Some(PropertyValue::LengthFeet(value))) => (*value, 0.3048),
            ("area", Some(PropertyValue::AreaSquareFeet(value))) => (*value, 0.09290304),
            ("volume", Some(PropertyValue::VolumeCubicFeet(value))) => (*value, 0.028316846592),
            _ => {
                panic!("saved dimensional specification must select the corresponding IFC measure")
            }
        };
        assert_eq!(typed, saved, "export preserves the saved native magnitude");
        let step = value.expect("typed saved property").to_step();
        let number: f64 = step
            .split_once('(')
            .expect("measure constructor")
            .1
            .trim_end_matches(')')
            .parse()
            .expect("STEP measure number");
        assert_eq!(
            number,
            saved * factor,
            "IFC measure preserves the converted SI magnitude"
        );
        checked += 1;
    }
    checked
}

fn check_pipe_dimensions(
    source: &[Parameter],
    sets: &[&PropertySet],
    definitions: &BTreeMap<i64, rvt::native_parameter_definitions::Definition>,
) -> usize {
    let mut checked = 0;
    for (id, name) in [
        (-1_140_225, "Diameter"),
        (-1_140_212, "Inside Diameter"),
        (-1_140_238, "Outside Diameter"),
        (-1_114_002, "Start Middle Elevation"),
        (-1_114_003, "End Middle Elevation"),
    ] {
        let definition = definitions.get(&id);
        // File-local definitions take precedence over the public built-in
        // profile. Other saved dimensions are checked by check_dimensions.
        if definition
            .and_then(|definition| definition.spec_type_id.as_deref())
            .and_then(|spec| spec.strip_prefix("autodesk.spec.aec:"))
            .is_some_and(|spec| spec.split('-').next() != Some("length"))
        {
            continue;
        }
        let name = definition
            .map(|definition| definition.caption.as_str())
            .filter(|caption| !caption.is_empty())
            .unwrap_or(name);
        let Some(saved) = unambiguous(source, id)
            .filter(|parameter| parameter.storage_type == "Double")
            .and_then(|parameter| parameter.raw_value.as_f64())
            .filter(|value| value.is_finite())
        else {
            continue;
        };
        let qualified = format!("{name} [{id}]");
        let value = property(sets, name).or_else(|| property(sets, &qualified));
        assert!(
            matches!(value, Some(PropertyValue::LengthFeet(value)) if *value == saved),
            "native pipe dimensions preserve their saved magnitude and length specification"
        );
        let step = value.expect("saved pipe dimension").to_step();
        let number: f64 = step
            .strip_prefix("IFCLENGTHMEASURE(")
            .and_then(|number| number.strip_suffix(')'))
            .expect("IFC length measure")
            .parse()
            .expect("STEP measure number");
        assert_eq!(
            number,
            saved * 0.3048,
            "pipe dimensions are SI measures, not project display units or geometry-derived quantities"
        );
        checked += 1;
    }
    checked
}

fn step_entities(step: &str) -> BTreeMap<usize, (&str, &str)> {
    step.lines()
        .filter_map(|line| {
            let (id, body) = line.strip_prefix('#')?.split_once('=')?;
            let (entity, args) = body.split_once('(')?;
            Some((id.parse().ok()?, (entity, args.strip_suffix(");")?)))
        })
        .collect()
}

fn step_args(args: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut depth = 0usize;
    for (index, character) in args.char_indices() {
        match character {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth = depth.saturating_sub(1),
            ',' if !quoted && depth == 0 => {
                fields.push(&args[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    fields.push(&args[start..]);
    fields
}

#[test]
fn current_saved_marks_and_dimensions_reach_occurrences_and_inherited_type_sets() {
    for fixture in common::project_fixtures::projects() {
        let mut file = RevitFile::open(fixture.path).expect("open authorized real project");
        let model = RvtDocExporter
            .export_with_mode_and_limits(
                &mut file,
                ExportQualityMode::TypedNoGeometry,
                Default::default(),
            )
            .expect("export real project metadata");
        let occurrences: BTreeMap<u32, usize> = model
            .entities
            .iter()
            .enumerate()
            .filter_map(|(index, entity)| match entity {
                IfcEntity::BuildingElement {
                    type_guid: Some(tag),
                    ..
                } => Some((tag.parse().ok()?, index)),
                _ => None,
            })
            .collect();
        let types: BTreeSet<_> = occurrences
            .keys()
            .filter_map(|id| model.element_type_ids.get(id).copied())
            .collect();
        let mut values = BTreeMap::new();
        let summary = native_document::extract(
            &mut file,
            &native_document::Options {
                selected_ids: occurrences
                    .keys()
                    .chain(&types)
                    .map(|id| u64::from(*id))
                    .collect(),
                ..Default::default()
            },
            |record| {
                let id =
                    u32::try_from(record.identity.element_id).expect("qualified current owner ID");
                values.insert(id, parameters_for_owner(record));
                Ok(())
            },
        )
        .expect("read actual current saved parameter values");
        let occurrence_sets = |index| -> Vec<&PropertySet> {
            model
                .entities
                .iter()
                .filter_map(|entity| match entity {
                    IfcEntity::ElementPropertySet { element, set } if *element == index => {
                        Some(set)
                    }
                    _ => None,
                })
                .collect()
        };
        let type_sets = |id| -> Vec<&PropertySet> {
            model
                .entities
                .iter()
                .filter_map(|entity| match entity {
                    IfcEntity::TypePropertySet { type_id, set } if *type_id == id => Some(set),
                    _ => None,
                })
                .collect()
        };
        let mut marks = 0;
        let mut dimensional_values = 0;
        let mut pipe_dimensions = 0;
        for (id, index) in &occurrences {
            let sets = occurrence_sets(*index);
            let source = values.get(id).map(Vec::as_slice).unwrap_or_default();
            let saved_mark = unambiguous(source, -1_001_203)
                .filter(|parameter| parameter.storage_type == "String")
                .and_then(|parameter| parameter.raw_value.as_str());
            match saved_mark {
                Some(mark) => {
                    assert!(
                        matches!(property(&sets, "Mark"), Some(PropertyValue::Text(value)) if value == mark),
                        "saved instance Mark reaches the same physical IFC occurrence"
                    );
                    marks += usize::from(!mark.is_empty());
                }
                None => assert!(
                    property(&sets, "Mark").is_none(),
                    "no instance Mark is fabricated from its identity or type"
                ),
            }
            dimensional_values += check_dimensions(source, &sets, &summary.parameter_definitions);
            pipe_dimensions += check_pipe_dimensions(source, &sets, &summary.parameter_definitions);
        }
        eprintln!("nonempty saved physical Marks checked: {marks}");
        let mut type_marks = 0;
        for id in &types {
            let sets = type_sets(*id);
            if let Some(source) = values.get(id) {
                dimensional_values +=
                    check_dimensions(source, &sets, &summary.parameter_definitions);
            }
            if matches!(property(&sets, "Type Mark"), Some(PropertyValue::Text(mark)) if !mark.is_empty())
            {
                type_marks += 1;
            }
            if let Some(mark) = values
                .get(id)
                .and_then(|values| unambiguous(values, -1_001_405))
                .filter(|parameter| parameter.storage_type == "String")
                .and_then(|parameter| parameter.raw_value.as_str())
            {
                assert!(
                    matches!(property(&sets, "Type Mark"), Some(PropertyValue::Text(value)) if value == mark),
                    "saved Type Mark reaches its IFC type rather than becoming an instance value"
                );
            }
        }
        assert!(
            dimensional_values > 0,
            "real instance/type custom dimensions use saved definitions"
        );
        if fixture.project == common::project_fixtures::Project::Mechanical {
            eprintln!("saved native pipe dimensions checked: {pipe_dimensions}");
            assert!(
                pipe_dimensions > 0,
                "measured pipe dimensions reach IFC occurrences"
            );
        }
        assert!(
            type_marks > 0,
            "real IFC type sets retain established nonempty Type Marks"
        );
        let step = rvt::ifc::write_step(&model);
        let written = step_entities(&step);
        let mut linked_types = 0;
        for (entity, args) in written.values() {
            if !entity.ends_with("TYPE") {
                continue;
            }
            let fields = step_args(args);
            let Some(type_id) = fields
                .get(7)
                .and_then(|field| field.trim_matches('\'').parse::<u32>().ok())
            else {
                continue;
            };
            if type_sets(type_id).is_empty() {
                continue;
            }
            let references = fields[5]
                .strip_prefix('(')
                .and_then(|refs| refs.strip_suffix(')'))
                .expect("IFC type HasPropertySets contains direct property-set references");
            for reference in references.split(',') {
                let id = reference
                    .strip_prefix('#')
                    .and_then(|id| id.parse::<usize>().ok())
                    .expect("STEP property-set reference");
                let (kind, args) = written[&id];
                assert_eq!(kind, "IFCPROPERTYSET");
                assert_eq!(step_args(args)[2], "'RvtParameters'");
            }
            linked_types += 1;
        }
        assert!(
            linked_types > 0,
            "real type properties are linked in serialized IFC"
        );
        assert!(
            !step.contains("IFCELEMENTQUANTITY("),
            "parameter export does not invent Qto quantities"
        );
    }
}
