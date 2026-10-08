//! Classify every unattributed placed frame against the schema-decoded current
//! index and graveyard. This is read-only evidence; no frame is exported and
//! no identity or routing gate is relaxed.
//!
//! Usage: cargo run --release --example probe_omitted_record_identity -- MODEL.rvt

use anyhow::{Context, Result, ensure};
use rvt::{
    RevitFile, elem_table, native_document, native_index, partition_element_records as per,
    schema_registry,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn identifier(mut value: &Value) -> Option<u64> {
    for _ in 0..8 {
        if let Some(id) = value.as_u64() {
            return Some(id);
        }
        value = value.get("m_id").or_else(|| value.get("m_id64"))?;
    }
    None
}

fn word(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn placed_volume(bytes: &[u8], offset: usize) -> bool {
    if word(bytes, offset + per::CONTAINER_OFFSET) != Some(per::CONTAINER_NONE)
        || !word(bytes, offset + per::PLACEMENT_KIND_OFFSET)
            .is_some_and(|kind| kind as u32 == per::PLACEMENT_KIND_INSTANCE)
    {
        return false;
    }
    let mut bounds = [0.0; 6];
    for (axis, value) in bounds.iter_mut().enumerate() {
        let Some(raw) = word(bytes, offset + per::BBOX_OFFSET + axis * 8) else {
            return false;
        };
        *value = f64::from_bits(raw);
        if !value.is_finite() {
            return false;
        }
    }
    (0..3).all(|axis| bounds[axis + 3] - bounds[axis] > per::VOLUME_EPSILON_FEET)
}

fn main() -> Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .context("usage: probe_omitted_record_identity MODEL.rvt")?;
    let mut file = RevitFile::open(path)?;
    let version = file.basic_file_info()?.version;
    let marker = per::file_bbox_marker(&mut file, version).context("unsupported record marker")?;
    let registry =
        schema_registry::parse(&native_document::read_single(&mut file, "Formats/Latest")?)?;
    let episodes = native_index::creation_episodes(
        &native_document::read_single(&mut file, "Global/History")?,
        &registry,
    )?;
    let native = native_index::parse(
        &native_document::read_single(&mut file, "Global/ElemTable")?,
        &registry,
        &episodes,
    )?;
    let mut graves = BTreeMap::<u64, usize>::new();
    let mut unknown_graveyard_rows = 0;
    for row in &native.graveyard_rows {
        if let Some(id) = identifier(&row["m_id"]) {
            *graves.entry(id).or_default() += 1;
        } else {
            unknown_graveyard_rows += 1;
        }
    }
    let declared = elem_table::declared_ids(&elem_table::parse_records(&mut file)?);
    let current_original_ids: BTreeSet<_> = native
        .identities
        .values()
        .map(|identity| identity.original_id_suffix)
        .collect();
    let categories: Vec<_> = per::RECOVERED_CATEGORIES
        .iter()
        .map(|(id, _)| *id)
        .collect();
    let mut frames = Vec::new();
    let mut by_classification = BTreeMap::<String, usize>::new();
    let mut by_class = BTreeMap::<String, usize>::new();
    let mut owners = BTreeSet::new();
    for stream in file.partition_stream_names() {
        let inflated = file.inflated_partition(&stream)?;
        let bytes = inflated.bytes();
        let chain = per::partition_record_chain(bytes, &marker);
        let assigned = per::assign_second_prologue_ids(bytes, &marker, &declared);
        let expected =
            per::count_unattributed_frames_excluding(bytes, &categories, &marker, &assigned);
        let mut observed = BTreeMap::<i64, usize>::new();
        for hit in memchr::memmem::find_iter(bytes, &marker) {
            let Some(offset) = hit.checked_sub(per::BBOX_MARKER_OFFSET) else {
                continue;
            };
            if assigned.contains_key(&offset)
                || !word(bytes, offset).is_some_and(per::carries_no_element_id)
                || !placed_volume(bytes, offset)
            {
                continue;
            }
            let Some(span) = per::enclosing_record(&chain, offset) else {
                continue;
            };
            let Some(category) = word(bytes, offset + per::CATEGORY_OFFSET).map(|word| word as i64)
            else {
                continue;
            };
            let Some((_, class)) = per::RECOVERED_CATEGORIES
                .iter()
                .find(|(id, _)| *id == category)
            else {
                continue;
            };
            let current = native.identities.contains_key(&span.element_id);
            let original = current_original_ids.contains(&span.element_id);
            let declared_id = u32::try_from(span.element_id).is_ok_and(|id| declared.contains(&id));
            let frame_within_record = offset
                .checked_add(per::BBOX_OFFSET + 48)
                .is_some_and(|end| end <= span.end);
            let grave_count = graves.get(&span.element_id).copied().unwrap_or(0);
            let classification = match (current || original || declared_id, grave_count) {
                (false, _) if !frame_within_record => "ambiguous_frame_boundary",
                (false, 1) => "graveyard_record",
                (false, 0) => "unresolved_identity",
                (false, _) => "ambiguous_graveyard_identity",
                (true, _) => "current_index_identity_requires_investigation",
            };
            *observed.entry(category).or_default() += 1;
            *by_classification.entry(classification.into()).or_default() += 1;
            *by_class.entry((*class).into()).or_default() += 1;
            owners.insert(span.element_id);
            frames.push(json!({
                "stream": stream,
                "frame_offset": offset,
                "record_start": span.start,
                "record_end": span.end,
                "element_id": span.element_id,
                "class": class,
                "current_index_contains_id": current,
                "current_original_ids_contains_id": original,
                "declared_ids_contains_id": declared_id,
                "frame_within_record": frame_within_record,
                "graveyard_rows_for_id": grave_count,
                "classification": classification,
            }));
        }
        ensure!(
            observed == expected,
            "omission reconciliation failed in {stream}"
        );
    }
    let diagnostics = rvt::ifc::build_export_diagnostics(
        &mut file,
        &rvt::ifc::IfcModel::default(),
        rvt::ifc::ExportDiagnosticsMode::Default,
    );
    let classes = |reason: &str| {
        diagnostics
            .skipped
            .iter()
            .find(|item| item.reason == reason)
            .map(|item| item.classes.clone())
            .unwrap_or_default()
    };
    let unresolved = classes("element_record_without_element_id");
    let graveyard = classes("element_record_in_graveyard");
    let warnings: Vec<_> = diagnostics
        .warnings
        .iter()
        .filter(|warning| warning.starts_with("Could not verify unattributed records"))
        .collect();
    let raw_total: usize = unresolved.values().sum::<usize>() + graveyard.values().sum::<usize>();
    ensure!(
        raw_total == frames.len(),
        "production classification changed the raw candidate count"
    );
    let proven = by_classification
        .get("graveyard_record")
        .copied()
        .unwrap_or(0);
    ensure!(
        graveyard.values().sum::<usize>() == proven,
        "production/probe graveyard classification mismatch"
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "revit_version": version,
            "current_index_records": native.identities.len(),
            "graveyard_rows": native.graveyard_rows.len(),
            "graveyard_unparsed_identity_rows": unknown_graveyard_rows,
            "graveyard_duplicate_ids": graves.values().filter(|count| **count > 1).count(),
            "graveyard_current_index_overlap": graves.keys().filter(|id| native.identities.contains_key(id)).count(),
            "omitted_frames": frames.len(),
            "unique_omitted_owner_ids": owners.len(),
            "by_classification": by_classification,
            "by_class": by_class,
            "all_counts_reconciled_with_production": true,
            "production_unresolved_by_class": unresolved,
            "production_graveyard_by_class": graveyard,
            "production_classification_warnings": warnings,
            "frames": frames,
        }))?
    );
    Ok(())
}
