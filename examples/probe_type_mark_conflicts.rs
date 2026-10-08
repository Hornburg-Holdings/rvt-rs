//! Compare RE-77 Type Mark text with current native owner parameter carriers.
//!
//! FACT: a conflict can be qualified by current record identity and the root's
//! unique serialized parameter pointers, rather than by an authored caption.
//! This probe emits only owner IDs, source offsets, flags, counts and equality
//! results. It never prints authored values, names or raw bytes. Reproduce on
//! each supported release corpus by providing a model and its selected type IDs;
//! unsupported graphs remain explicit and cannot qualify an absent value.
//!
//! Usage: cargo run --release --example probe_type_mark_conflicts -- MODEL.rvt TYPE_ID ...

use anyhow::{Context, Result, ensure};
use rvt::{RevitFile, native_document, native_parameters::ObjectGraph};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const TYPE_MARK: i64 = -1_001_405;

fn root_pointer_target(
    graph: &ObjectGraph,
    field: &str,
    expected_class: &str,
) -> Result<Option<usize>> {
    let root = graph.objects.first().context("native root object absent")?;
    let Some(pointer) = root.fields.get(field) else {
        return Ok(None);
    };
    let token = pointer["pointer_token"]
        .as_u64()
        .context("root parameter pointer token absent")?;
    if token == 0 {
        return Ok(None);
    }
    let offset = usize::try_from(
        pointer["offset"]
            .as_u64()
            .context("root parameter pointer offset absent")?,
    )?;
    let mut edges = graph
        .edges
        .iter()
        .filter(|edge| edge.source_object_index == 0 && edge.pointer_offset == offset);
    let edge = edges
        .next()
        .context("root parameter pointer has no local edge")?;
    ensure!(
        edges.next().is_none(),
        "root parameter pointer has multiple edges"
    );
    ensure!(
        u64::from(edge.pointer_token) == token,
        "root parameter pointer token disagrees with its edge"
    );
    let target = graph
        .objects
        .get(edge.target_object_index)
        .context("root parameter edge target outside graph")?;
    ensure!(
        target.class_name == expected_class,
        "root parameter pointer target class mismatch"
    );
    Ok(Some(edge.target_object_index))
}

fn value_state(value: &Value, legacy: Option<&str>) -> Value {
    let string = value.as_str();
    json!({
        "is_string": string.is_some(),
        "empty": string.map(str::is_empty),
        "nonempty": string.map(|value| !value.is_empty()),
        "trimmed_empty": string.map(|value| value.trim().is_empty()),
        "utf16_units": string.map(|value| value.encode_utf16().count()),
        "equals_re77": string.zip(legacy).map(|(value, legacy)| value == legacy),
        "trimmed_equals_re77": string.zip(legacy).map(|(value, legacy)| value.trim() == legacy.trim()),
    })
}

fn pointer_state(pointer: &Value) -> &'static str {
    match pointer["pointer_token"].as_u64() {
        Some(0) => "null",
        Some(_) => "nonnull",
        None => "unknown",
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .context("usage: probe_type_mark_conflicts MODEL.rvt TYPE_ID ...")?;
    let mut ids = BTreeSet::<u64>::new();
    for argument in args {
        let id: u32 = argument
            .to_str()
            .context("type ID is not UTF-8")?
            .parse()
            .context("type ID is not a positive u32")?;
        ensure!(
            id != 0 && ids.insert(u64::from(id)),
            "type IDs must be positive and unique"
        );
    }
    ensure!(!ids.is_empty(), "at least one type ID is required");
    let mut file = RevitFile::open(path)?;
    let version = file.basic_file_info()?.version;
    let legacy = rvt::partition_type_parameters::type_text_parameters(&mut file, version);
    let mut records = BTreeMap::<u64, Value>::new();
    let summary = native_document::extract(
        &mut file,
        &native_document::Options {
            selected_ids: ids,
            ..Default::default()
        },
        |record| {
            let id = record.identity.element_id;
            ensure!(
                !records.contains_key(&id),
                "selected owner has multiple current records"
            );
            let legacy = u32::try_from(id)
                .ok()
                .and_then(|id| legacy.get(&id))
                .and_then(|parameters| parameters.get("Type Mark"))
                .map(String::as_str);
            let mut values = Vec::<Value>::new();
            let mut sources = Vec::<Value>::new();
            let mut other_graph_slots = 0usize;
            let mut excluded_instance_defaults = 0usize;
            if let (Some(graph), Some(metadata)) = (&record.graph, &record.saved_metadata) {
                for (field, class, storage) in [
                    ("m_pParamValueSetDouble", "ParamValueSetDouble", "Double"),
                    ("m_pParamValueSetInt", "ParamValueSetInt", "Integer"),
                    ("m_pParamValueSetAString", "ParamValueSetAString", "String"),
                    (
                        "m_pParamValueSetElementId",
                        "ParamValueSetElementId",
                        "ElementId",
                    ),
                ] {
                    let Some(owner) = root_pointer_target(graph, field, class)? else {
                        continue;
                    };
                    for set in metadata
                        .parameter_sets
                        .iter()
                        .filter(|set| set.source_object == owner && set.root_pointer_field == field)
                    {
                        for parameter in set
                            .parameters
                            .iter()
                            .filter(|parameter| parameter.serialized_parameter_id == TYPE_MARK)
                        {
                            ensure!(
                                parameter.storage_type == storage,
                                "native typed parameter set storage mismatch"
                            );
                            values.push(parameter.raw_value.clone());
                            sources.push(json!({
                                "carrier": "root_typed_set",
                                "qualified_value_index": values.len() - 1,
                                "root_pointer_field": field,
                                "source_object": owner,
                                "object_body_offset": graph.objects[owner].start,
                                "object_fields_end": graph.objects[owner].fields_end,
                                "storage_type": storage,
                                "value_state": value_state(&parameter.raw_value, legacy),
                            }));
                        }
                    }
                }
                let root = graph.objects.first().context("native root object absent")?;
                let family_pointer = match root.class_name.as_str() {
                    "FamilySymbol" => Some(("m_pParams", false)),
                    "FamilyInstance" => Some(("m_pInstParams", true)),
                    _ => None,
                };
                let family_owner = family_pointer
                    .map(|(field, instance)| {
                        root_pointer_target(graph, field, "FamilyParams")
                            .map(|owner| owner.map(|owner| (owner, instance)))
                    })
                    .transpose()?
                    .flatten();
                for slot in metadata
                    .family_parameter_slots
                    .iter()
                    .filter(|slot| slot.parameter_id == TYPE_MARK)
                {
                    let root_owned =
                        family_owner.is_some_and(|(owner, _)| slot.source_object == owner);
                    if !root_owned {
                        other_graph_slots += 1;
                        continue;
                    }
                    let eligible = family_owner
                        .is_some_and(|(_, instance)| instance || !slot.stored_instance_flag);
                    excluded_instance_defaults += usize::from(!eligible);
                    if eligible && slot.string_slot.is_string() {
                        values.push(slot.string_slot.clone());
                    }
                    sources.push(json!({
                    "carrier": "root_family_slot",
                    "qualified_value_index": if eligible && slot.string_slot.is_string() { Some(values.len() - 1) } else { None },
                    "source_object": slot.source_object,
                    "object_body_offset": graph.objects[slot.source_object].start,
                    "object_fields_end": graph.objects[slot.source_object].fields_end,
                    "stored_instance_flag": slot.stored_instance_flag,
                    "reporting": slot.reporting,
                    "expression_pointer": pointer_state(&slot.expression_pointer),
                    "export_eligible": eligible && slot.string_slot.is_string(),
                    "value_state": value_state(&slot.string_slot, legacy),
                }));
                }
            }
            let mut pairwise = Vec::new();
            for (left, value) in values.iter().enumerate() {
                for (right, other) in values.iter().enumerate().skip(left + 1) {
                    pairwise.push(json!({"left":left,"right":right,"equal":value == other}));
                }
            }
            let native_conflict = pairwise.iter().any(|pair| pair["equal"] == false);
            let legacy_conflict = legacy
                .is_some_and(|legacy| values.iter().any(|value| value.as_str() != Some(legacy)));
            records.insert(
                id,
                json!({
                    "owner_id": id,
                    "class": record.class_name,
                    "graph_status": record.status,
                    "has_graph": record.graph.is_some(),
                    "has_metadata": record.saved_metadata.is_some(),
                    "metadata_diagnostic_present": record.metadata_diagnostic.is_some(),
                    "stream": record.source.stream,
                    "group_first_marker_offset": record.source.group.first_marker_offset,
                    "group_channel": record.source.group.channel,
                    "group_segment_count": record.source.group.segment_count,
                    "group_record_offset": record.source.group_record_offset,
                    "record_body_bytes": record.source.body_bytes,
                    "re77_present": legacy.is_some(),
                    "re77_nonempty": legacy.map(|value| !value.is_empty()),
                    "re77_utf16_units": legacy.map(|value| value.encode_utf16().count()),
                    "qualified_native_values": values.len(),
                    "native_sources": sources,
                    "native_pairwise_equality": pairwise,
                    "native_internal_conflict": native_conflict,
                    "native_re77_conflict": legacy_conflict,
                    "other_graph_slots_excluded": other_graph_slots,
                    "instance_defaults_excluded": excluded_instance_defaults,
                }),
            );
            Ok(())
        },
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "version": summary.revit_version,
            "parameter_id": TYPE_MARK,
            "owners": records,
            "absent_from_index": summary.requested_ids_absent_from_index,
            "selected_ids_without_records": summary.selected_ids_without_records,
        }))?
    );
    Ok(())
}
