//! Census schema-directed parameter recovery on an authorized real RVT file.
//!
//! FACT: independently bounded current records either expose saved
//! scalar parameter values and definition-resolved family slots, or report the
//! precise native graph/metadata refusal. Authored values and captions are never
//! printed. Counts include only current owners selected by the native index.
//! Verify across the release corpus by passing each RVT/RFA path explicitly;
//! releases outside the native current-record contract fail explicitly.
//!
//! Usage: cargo run --release --example probe_native_parameter_recovery -- MODEL.rvt
//! [--channel 102|103] [--max-graph-values N]

use anyhow::{Context, Result, ensure};
use rvt::{RevitFile, native_document, native_parameters::ObjectGraph};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

// The first five are the exporter's narrow public quantity mappings. The
// following five are direct native pipe fields, followed by type dimensions
// established in RE-93/94/103. An ID here only requests a count: it never
// selects a FamilyParams value slot or enables a definition.
const KNOWN_DIMENSION_IDS: [i64; 20] = [
    -1_004_005,
    -1_155_247,
    -1_001_953,
    -1_012_805,
    -1_012_806,
    -1_140_225,
    -1_140_212,
    -1_140_238,
    -1_114_002,
    -1_114_003,
    rvt::partition_type_parameters::WINDOW_WIDTH_PARAMETER,
    rvt::partition_type_parameters::WINDOW_HEIGHT_PARAMETER,
    rvt::partition_type_parameters::DOOR_ROUGH_WIDTH_PARAMETER,
    rvt::partition_type_parameters::DOOR_ROUGH_HEIGHT_PARAMETER,
    rvt::partition_type_parameters::SECTION_WIDTH_PARAMETER,
    rvt::partition_type_parameters::SECTION_HEIGHT_PARAMETER,
    rvt::partition_type_parameters::SECTION_WEB_THICKNESS_PARAMETER,
    rvt::partition_type_parameters::SECTION_FLANGE_THICKNESS_PARAMETER,
    rvt::partition_type_parameters::SECTION_CENTROID_HORIZONTAL_PARAMETER,
    rvt::partition_type_parameters::SECTION_CENTROID_VERTICAL_PARAMETER,
];

#[derive(Default, Serialize)]
struct FamilySlotCounts {
    slots: usize,
    root_owner_slots: usize,
    other_graph_slots: usize,
    stored_instance_flag: usize,
    stored_type_flag: usize,
    reporting: usize,
    null_expression_pointer: usize,
    nonnull_expression_pointer: usize,
    unknown_expression_pointer: usize,
    finite_double_slot: usize,
    nonzero_double_slot: usize,
    positive_double_slot: usize,
    nonzero_integer_slot: usize,
    nonempty_string_slot: usize,
    nonzero_element_id_slot: usize,
}

// Qualify ownership using the actual root pointer's unique native graph edge,
// without searching nested FamilyParams objects or matching an authored name.
fn root_family_owner(graph: &ObjectGraph) -> (&'static str, Option<usize>) {
    let Some(root) = graph.objects.first() else {
        return ("missing_root", None);
    };
    let field = match root.class_name.as_str() {
        "Family" => "m_familyParams",
        "FamilySymbol" => "m_pParams",
        "FamilyInstance" => "m_pInstParams",
        _ => return ("not_family_owner", None),
    };
    let Some(pointer) = root.fields.get(field) else {
        return ("missing_pointer_field", None);
    };
    match pointer["pointer_token"].as_u64() {
        Some(0) => return ("null_pointer", None),
        Some(_) => {}
        None => return ("invalid_pointer_token", None),
    }
    let Some(offset) = pointer["offset"]
        .as_u64()
        .and_then(|offset| usize::try_from(offset).ok())
    else {
        return ("invalid_pointer_offset", None);
    };
    let mut edges = graph
        .edges
        .iter()
        .filter(|edge| edge.source_object_index == 0 && edge.pointer_offset == offset);
    let Some(edge) = edges.next() else {
        return ("missing_graph_edge", None);
    };
    if edges.next().is_some() {
        return ("ambiguous_graph_edge", None);
    }
    if graph
        .objects
        .get(edge.target_object_index)
        .is_none_or(|object| object.class_name != "FamilyParams")
    {
        return ("wrong_target_class", None);
    }
    ("qualified_root_edge", Some(edge.target_object_index))
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .context("usage: probe_native_parameter_recovery MODEL.rvt")?;
    let mut options = native_document::Options::default();
    while let Some(option) = args.next() {
        let value = args.next().context("probe option needs a value")?;
        let value = value.to_str().context("probe option value must be UTF-8")?;
        match option.to_str() {
            Some("--channel") => {
                let channel = value.parse()?;
                ensure!(matches!(channel, 102 | 103), "unsupported probe channel");
                options.channels = BTreeSet::from([channel]);
            }
            Some("--max-graph-values") => {
                options.max_graph_values = value.parse()?;
                ensure!(
                    options.max_graph_values > 0,
                    "graph budget must be positive"
                );
            }
            _ => anyhow::bail!("unknown probe option"),
        }
    }
    let mut file = RevitFile::open(path)?;
    let mut classes = BTreeMap::<String, serde_json::Value>::new();
    let mut parameters = BTreeMap::<i64, BTreeMap<String, usize>>::new();
    let mut graph_failures = BTreeMap::<String, usize>::new();
    let mut refused_owner_examples = BTreeMap::<String, Vec<serde_json::Value>>::new();
    let mut metadata_failures = BTreeMap::<String, usize>::new();
    let mut marked_owners = 0usize;
    let mut type_marked_owners = 0usize;
    let mut typed_scalar_values = 0usize;
    let mut resolved_family_values = 0usize;
    let mut resolved_family_storage_counts = BTreeMap::<String, usize>::new();
    let mut root_family_edge_counts = BTreeMap::<String, BTreeMap<String, usize>>::new();
    let mut dimension_family_slots =
        BTreeMap::<i64, BTreeMap<String, BTreeMap<String, FamilySlotCounts>>>::new();
    let mut dimension_typed_sources =
        BTreeMap::<i64, BTreeMap<String, BTreeMap<String, usize>>>::new();
    let mut graph_parameter_set_counts = BTreeMap::<String, usize>::new();
    let mut graph_dimension_typed_sources =
        BTreeMap::<i64, BTreeMap<String, BTreeMap<String, usize>>>::new();
    let mut direct_dimension_sources = BTreeMap::<i64, BTreeMap<String, usize>>::new();
    let summary = native_document::extract(&mut file, &options, |record| {
        let class = record.class_name.as_deref().unwrap_or("<unregistered>");
        let counts = classes.entry(class.into()).or_insert_with(|| {
                json!({"owners":0,"owners_with_metadata":0,"typed_scalar_values":0,"resolved_family_values":0,"owners_with_nonempty_mark":0,"owners_with_nonempty_type_mark":0})
            });
        counts["owners"] = json!(counts["owners"].as_u64().unwrap_or(0) + 1);
        if let Some(diagnostic) = record.diagnostic {
            // Offsets vary by body. Group by the actual decoder refusal.
            let refusal = diagnostic.rsplit(": ").next().unwrap_or(&diagnostic);
            *graph_failures.entry(refusal.into()).or_default() += 1;
            let examples = refused_owner_examples.entry(refusal.into()).or_default();
            if examples.len() < 8 {
                examples.push(json!({
                    "element_id": record.identity.element_id,
                    "class": class,
                    "stream": record.source.stream,
                    "group_marker_offset": record.source.group.first_marker_offset,
                    "group_record_offset": record.source.group_record_offset,
                    "body_bytes": record.source.body_bytes,
                }));
            }
        }
        if let Some(diagnostic) = record.metadata_diagnostic {
            *metadata_failures.entry(diagnostic).or_default() += 1;
        }
        let (edge_status, family_owner) = record
            .graph
            .as_ref()
            .map(root_family_owner)
            .unwrap_or(("unsupported_graph", None));
        *root_family_edge_counts
            .entry(class.into())
            .or_default()
            .entry(edge_status.into())
            .or_default() += 1;
        // Inspect every declared typed set, including nested graph nodes.
        // A hit here establishes a saved carrier, not root ownership or
        // permission to use a nested value as an occurrence quantity.
        if let Some(graph) = &record.graph {
            for object in &graph.objects {
                if !matches!(
                    object.class_name.as_str(),
                    "ParamValueSetDouble"
                        | "ParamValueSetInt"
                        | "ParamValueSetAString"
                        | "ParamValueSetElementId"
                ) {
                    continue;
                }
                let pairs = object.fields["m_paramSet"]
                    .as_array()
                    .context("complete typed set has no parameter pairs")?;
                *graph_parameter_set_counts
                    .entry(object.class_name.clone())
                    .or_default() += pairs.len();
                for pair in pairs {
                    let mut id = &pair["m_paramId"];
                    while let Some(inner) = id.get("m_id").or_else(|| id.get("m_id64")) {
                        id = inner;
                    }
                    let id = id
                        .as_i64()
                        .context("complete typed set has invalid parameter ID")?;
                    if KNOWN_DIMENSION_IDS.contains(&id) {
                        *graph_dimension_typed_sources
                            .entry(id)
                            .or_default()
                            .entry(class.into())
                            .or_default()
                            .entry(object.class_name.clone())
                            .or_default() += 1;
                    }
                }
            }
        }
        let Some(metadata) = record.saved_metadata else {
            return Ok(());
        };
        counts["owners_with_metadata"] =
            json!(counts["owners_with_metadata"].as_u64().unwrap_or(0) + 1);
        let mut mark = false;
        let mut type_mark = false;
        for parameter in metadata
            .parameter_sets
            .iter()
            .flat_map(|set| &set.parameters)
        {
            typed_scalar_values += 1;
            counts["typed_scalar_values"] =
                json!(counts["typed_scalar_values"].as_u64().unwrap_or(0) + 1);
            *parameters
                .entry(parameter.serialized_parameter_id)
                .or_default()
                .entry(parameter.storage_type.clone())
                .or_default() += 1;
            let nonempty = parameter.raw_value.as_str().is_some_and(|v| !v.is_empty());
            mark |= parameter.serialized_parameter_id == -1001203 && nonempty;
            type_mark |= parameter.serialized_parameter_id == -1001405 && nonempty;
        }
        for set in &metadata.parameter_sets {
            for parameter in &set.parameters {
                if KNOWN_DIMENSION_IDS.contains(&parameter.serialized_parameter_id) {
                    *dimension_typed_sources
                        .entry(parameter.serialized_parameter_id)
                        .or_default()
                        .entry(class.into())
                        .or_default()
                        .entry(format!(
                            "{}:{}",
                            set.root_pointer_field, parameter.storage_type
                        ))
                        .or_default() += 1;
                }
            }
        }
        for parameter in &metadata.raw_field_parameters {
            if parameter.projection_rule == "direct_saved_field"
                && parameter
                    .source_element_id
                    .is_none_or(|id| u64::try_from(id).ok() == Some(record.identity.element_id))
                && KNOWN_DIMENSION_IDS.contains(&parameter.parameter_id)
            {
                *direct_dimension_sources
                    .entry(parameter.parameter_id)
                    .or_default()
                    .entry(parameter.source_field.clone())
                    .or_default() += 1;
            }
        }
        for slot in &metadata.family_parameter_slots {
            if !KNOWN_DIMENSION_IDS.contains(&slot.parameter_id) {
                continue;
            }
            let root_owned = family_owner == Some(slot.source_object);
            let counts = dimension_family_slots
                .entry(slot.parameter_id)
                .or_default()
                .entry(class.into())
                .or_default()
                .entry(
                    if root_owned {
                        "root_owner"
                    } else {
                        "other_graph_object"
                    }
                    .into(),
                )
                .or_default();
            counts.slots += 1;
            counts.root_owner_slots += usize::from(root_owned);
            counts.other_graph_slots += usize::from(!root_owned);
            counts.stored_instance_flag += usize::from(slot.stored_instance_flag);
            counts.stored_type_flag += usize::from(!slot.stored_instance_flag);
            counts.reporting += usize::from(slot.reporting);
            match slot.expression_pointer["pointer_token"].as_u64() {
                Some(0) => counts.null_expression_pointer += 1,
                Some(_) => counts.nonnull_expression_pointer += 1,
                None => counts.unknown_expression_pointer += 1,
            }
            if let Some(value) = slot.double_slot.as_f64().filter(|value| value.is_finite()) {
                counts.finite_double_slot += 1;
                counts.nonzero_double_slot += usize::from(value != 0.);
                counts.positive_double_slot += usize::from(value > 0.);
            }
            counts.nonzero_integer_slot +=
                usize::from(slot.integer_slot.as_i64().is_some_and(|value| value != 0));
            counts.nonempty_string_slot += usize::from(
                slot.string_slot
                    .as_str()
                    .is_some_and(|value| !value.is_empty()),
            );
            counts.nonzero_element_id_slot += usize::from(slot.element_id_slot != 0);
        }
        marked_owners += usize::from(mark);
        type_marked_owners += usize::from(type_mark);
        counts["owners_with_nonempty_mark"] =
            json!(counts["owners_with_nonempty_mark"].as_u64().unwrap_or(0) + u64::from(mark));
        counts["owners_with_nonempty_type_mark"] = json!(
            counts["owners_with_nonempty_type_mark"]
                .as_u64()
                .unwrap_or(0)
                + u64::from(type_mark)
        );
        resolved_family_values += metadata.resolved_family_parameters.len();
        for parameter in &metadata.resolved_family_parameters {
            *resolved_family_storage_counts
                .entry(parameter.storage_type.clone())
                .or_default() += 1;
        }
        counts["resolved_family_values"] = json!(
            counts["resolved_family_values"].as_u64().unwrap_or(0)
                + metadata.resolved_family_parameters.len() as u64
        );
        Ok(())
    })?;
    let mut definition_storage_counts = BTreeMap::<String, usize>::new();
    let mut definition_spec_counts = BTreeMap::<String, usize>::new();
    for definition in summary.parameter_definitions.values() {
        *definition_storage_counts
            .entry(
                definition
                    .storage_type
                    .as_deref()
                    .unwrap_or("unresolved")
                    .into(),
            )
            .or_default() += 1;
        *definition_spec_counts
            .entry(
                definition
                    .spec_type_id
                    .as_deref()
                    .unwrap_or("unresolved")
                    .into(),
            )
            .or_default() += 1;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "revit_version":summary.revit_version,
            "schema_sha256":summary.schema_sha256,
            "current_owners":summary.emitted_records,
            "complete_graphs":summary.complete_graph_records,
            "unsupported_graphs":summary.unsupported_graph_records,
            "projected_metadata_owners":summary.projected_metadata_records,
            "unsupported_metadata_owners":summary.unsupported_metadata_records,
            "inflated_partition_bytes":summary.partitions.values().map(|p| p.inflated_bytes).sum::<u64>(),
            "definitions":summary.parameter_definitions.len(),
            "definition_refusals":summary.definition_diagnostics.len(),
            "builtin_catalog_provenance":summary.builtin_catalog_provenance,
            "typed_scalar_values":typed_scalar_values,
            "resolved_family_values":resolved_family_values,
            "resolved_family_storage_counts":resolved_family_storage_counts,
            "definition_storage_counts":definition_storage_counts,
            "definition_spec_counts":definition_spec_counts,
            "known_dimension_parameter_ids":KNOWN_DIMENSION_IDS,
            "known_dimension_family_slots":dimension_family_slots,
            "known_dimension_typed_sources":dimension_typed_sources,
            "channels": options.channels,
            "max_graph_values": options.max_graph_values,
            "all_graph_typed_set_value_counts": graph_parameter_set_counts,
            "all_graph_known_dimension_typed_sources": graph_dimension_typed_sources,
            "direct_saved_dimension_sources": direct_dimension_sources,
            "family_slot_counts_select_no_storage_type":true,
            "root_family_edge_counts":root_family_edge_counts,
            "owners_with_nonempty_mark":marked_owners,
            "owners_with_nonempty_type_mark":type_marked_owners,
            "classes":classes,
            "parameter_storage_counts":parameters,
            "graph_refusals":graph_failures,
            "refused_owner_examples":refused_owner_examples,
            "metadata_refusals":metadata_failures
        }))?
    );
    Ok(())
}
