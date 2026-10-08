//! Inspect competing placed-instance records on an authorized real project.
//!
//! Reports where comparing partition names as text chooses a different record
//! from comparing their numeric sequence. Both retain the existing greatest
//! vertical-extent rule. No authored names, parameters or raw bytes are emitted.
//!
//! Usage: cargo run --release --example probe_partition_record_recency -- MODEL.rvt [CATEGORY_ID ...]

use anyhow::{Context, Result};
use rvt::{RevitFile, elem_table, partition_element_records as per};
use serde_json::json;
use std::collections::BTreeMap;

fn z_extent(record: &per::PartitionElementRecord) -> i64 {
    let (_, _, height) = record.extents_feet();
    (height * 10_000.0).round() as i64
}

fn partition(record: &per::PartitionElementRecord) -> u32 {
    record
        .stream
        .rsplit('/')
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .context("usage: probe_partition_record_recency MODEL.rvt [CATEGORY_ID ...]")?;
    let extra_categories: Vec<i64> = args
        .map(|value| {
            value
                .to_str()
                .context("category is not UTF-8")?
                .parse()
                .context("category is not an integer")
        })
        .collect::<Result<_>>()?;
    let mut file = RevitFile::open(path)?;
    let version = file.basic_file_info()?.version;
    anyhow::ensure!(
        version >= 2024,
        "this probe compares the Revit 2024+ greatest-height selection rule"
    );
    let declared = elem_table::declared_ids(&elem_table::parse_records(&mut file)?);
    let mut categories: Vec<_> = per::RECOVERED_CATEGORIES
        .iter()
        .map(|(id, _)| *id)
        .collect();
    for category in &extra_categories {
        if !categories.contains(category) {
            categories.push(*category);
        }
    }
    let records = per::scan_category_records_multi(&mut file, version, &categories, &declared)?;
    let mut grouped = BTreeMap::<u32, Vec<&per::PartitionElementRecord>>::new();
    for record in &records {
        if record.is_exported_instance() && record.has_volume() {
            grouped.entry(record.element_id).or_default().push(record);
        }
    }
    let mut changed = Vec::new();
    let mut changed_bounds = 0;
    for (id, rows) in &grouped {
        let old = rows
            .iter()
            .max_by_key(|r| (z_extent(r), r.stream.as_str(), r.offset))
            .context("record group")?;
        let new = rows
            .iter()
            .max_by_key(|r| (z_extent(r), partition(r), r.offset))
            .context("record group")?;
        if old.stream == new.stream && old.offset == new.offset {
            continue;
        }
        let delta = old
            .bbox_feet
            .iter()
            .zip(new.bbox_feet)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        changed_bounds += usize::from(delta > 1e-8);
        changed.push(json!({
            "element_id": id,
            "category": new.builtin_category,
            "old_stream": old.stream,
            "new_stream": new.stream,
            "old_offset": old.offset,
            "new_offset": new.offset,
            "old_bbox_feet": old.bbox_feet,
            "new_bbox_feet": new.bbox_feet,
            "max_bbox_delta_feet": delta,
            "candidate_records": rows.len(),
        }));
    }
    let extra_admissions: Vec<_> = extra_categories
        .iter()
        .map(|category| {
            let candidates: Vec<_> = records
                .iter()
                .filter(|record| record.builtin_category == *category)
                .cloned()
                .collect();
            let raw_records = candidates.len();
            let decoded = rvt::partition_schema_mvp::instances_from_records(
                candidates,
                "Probe",
                &Default::default(),
            );
            json!({
                "category": category,
                "raw_records": raw_records,
                "admitted_ids": decoded.iter().filter_map(|element| element.id).collect::<Vec<_>>(),
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "placed_element_ids": grouped.len(),
            "changed_record_selections": changed.len(),
            "changed_bounds": changed_bounds,
            "changed": changed,
            "extra_category_admissions": extra_admissions,
        }))?
    );
    Ok(())
}
