//! The native record path emits a current record for every element the
//! index declares (#548).
//!
//! On Einhoven (2023) and Core Interior (2024) every element
//! `Global/ElemTable` declares has a channel-101 record in some partition
//! (RE-145: 2,615 of 2,615 and 26,425 of 26,425), so `native_document::extract`
//! over channel 101 must emit one record for each of them, and report no
//! selected element without one. An element whose stored revision routes to
//! a partition that holds none of its records is still a current element.
//!
//! Corpus resolution follows `tests/elem_table_frame.rs`: a configured but
//! incomplete project corpus fails, an absent one skips and says so.

use rvt::{RevitFile, native_document, native_index, native_parameters, schema_registry};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

mod common;

use common::project_fixtures::{self, Project};

fn project_file(name: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("RVT_PROJECT_CORPUS_DIR").map(PathBuf::from)?;
    let path = dir.join(name);
    assert!(
        path.exists(),
        "RVT_PROJECT_CORPUS_DIR is set to {} but {name} is not there",
        dir.display()
    );
    Some(path)
}

#[test]
fn every_declared_element_has_its_current_record_emitted() {
    let mut read = 0;
    for name in ["Revit_IFC5_Einhoven.rvt", "2024_Core_Interior.rvt"] {
        let Some(path) = project_file(name) else {
            continue;
        };
        read += 1;
        let mut rf = RevitFile::open(&path).expect("open");
        let options = native_document::Options {
            channels: BTreeSet::from([101]),
            ..native_document::Options::default()
        };
        let mut emitted = BTreeSet::new();
        let summary = native_document::extract(&mut rf, &options, |record| {
            emitted.insert(record.identity.element_id);
            Ok(())
        })
        .unwrap_or_else(|error| panic!("{name}: extract: {error:#}"));
        assert!(
            summary.selected_ids_without_records.is_empty(),
            "{name}: {} declared elements have no emitted record: {:?}",
            summary.selected_ids_without_records.len(),
            &summary.selected_ids_without_records
                [..summary.selected_ids_without_records.len().min(20)]
        );
        assert_eq!(
            emitted.len(),
            summary.indexed_elements,
            "{name}: one emitted record per declared element"
        );
    }
    if read == 0 {
        eprintln!("skipping: RVT_PROJECT_CORPUS_DIR is not set");
    }
}

/// Optional private-corpus regression for a real table whose two increment
/// vectors together exceed one object's value budget. The input stays external;
/// no customer bytes, names or paths are embedded in this check.
#[test]
fn large_increment_history_keeps_routing_bounded_and_complete() {
    let Some(path) = project_fixtures::project(Project::Structural) else {
        return;
    };
    let mut rf = RevitFile::open(&path).expect("open large increment project");
    let registry = schema_registry::parse(
        &native_document::read_single(&mut rf, "Formats/Latest").expect("read schema"),
    )
    .expect("parse schema");
    let bytes = native_document::read_single(&mut rf, "Global/DocumentIncrementTable")
        .expect("read increment table");
    let tag = u16::from_le_bytes(bytes[..2].try_into().expect("root tag"));
    let previous = native_parameters::decode_object_fields(&bytes, &registry, 2, tag)
        .expect_err("fixture must exercise the former cumulative value limit");
    assert!(
        previous
            .to_string()
            .contains("native field value budget exceeded (100000)"),
        "unexpected legacy decode refusal: {previous:#}"
    );
    let increments = native_index::storage_increments(&bytes, &registry)
        .expect("decode large increment table row by row");
    assert!(!increments.is_empty());

    let episodes = native_index::creation_episodes(
        &native_document::read_single(&mut rf, "Global/History").expect("read history"),
        &registry,
    )
    .expect("decode history");
    let index = native_index::parse(
        &native_document::read_single(&mut rf, "Global/ElemTable").expect("read element table"),
        &registry,
        &episodes,
    )
    .expect("decode element index");
    let present: BTreeSet<u32> = rf
        .stream_names()
        .iter()
        .filter_map(|name| {
            name.strip_prefix("Partitions/")
                .and_then(|n| n.parse().ok())
        })
        .collect();
    assert!(!index.identities.is_empty());
    let revisions: BTreeSet<_> = index
        .identities
        .values()
        .map(|identity| identity.stored_revision)
        .collect();
    for revision in revisions {
        native_index::route_episode(revision, &increments, &present)
            .expect("every indexed element retains a valid physical route");
    }

    let mut excessive = bytes.clone();
    excessive[2..6].copy_from_slice(&100_001u32.to_le_bytes());
    assert!(
        native_index::storage_increments(&excessive, &registry)
            .expect_err("oversized increment vector must be refused")
            .to_string()
            .contains("native container count budget exceeded")
    );
    assert!(native_index::storage_increments(&bytes[..bytes.len() - 1], &registry).is_err());
    let mut trailing = bytes;
    trailing.push(0);
    assert!(native_index::storage_increments(&trailing, &registry).is_err());
}

fn raw_project_index(file: &mut RevitFile) -> native_index::ProjectIndex {
    let registry = schema_registry::parse(
        &native_document::read_single(file, "Formats/Latest").expect("read real schema"),
    )
    .expect("parse real schema");
    let episodes = native_index::creation_episodes(
        &native_document::read_single(file, "Global/History").expect("read real history"),
        &registry,
    )
    .expect("decode real history");
    native_index::parse(
        &native_document::read_single(file, "Global/ElemTable").expect("read real index"),
        &registry,
        &episodes,
    )
    .expect("decode public raw index")
}

#[test]
fn compact_routing_preserves_raw_index_counts_and_selected_identities() {
    for fixture in project_fixtures::projects() {
        let mut file = RevitFile::open(&fixture.path).expect("open configured project");
        let raw = raw_project_index(&mut file);
        let selected: BTreeSet<_> = raw.identities.keys().copied().take(16).collect();
        let mut emitted = BTreeSet::new();
        let summary = native_document::extract_graphs(
            &mut file,
            &native_document::Options {
                channels: BTreeSet::from([101]),
                selected_ids: selected.clone(),
                ..Default::default()
            },
            |record| {
                assert!(selected.contains(&record.identity.element_id));
                let expected = &raw.identities[&record.identity.element_id];
                assert_eq!(
                    serde_json::to_value(&record.identity).expect("serialize emitted identity"),
                    serde_json::to_value(expected).expect("serialize raw identity"),
                    "compact routing changed saved identity fields"
                );
                assert!(emitted.insert(record.identity.element_id));
                Ok(())
            },
        )
        .expect("extract selected current records using compact preamble");
        assert!(
            !emitted.is_empty(),
            "real selection must emit current records"
        );
        assert_eq!(summary.indexed_elements, raw.identities.len());
        assert_eq!(summary.graveyard_records, raw.graveyard_rows.len());
        assert_eq!(summary.selected_indexed_elements, selected.len());
        assert_eq!(
            summary.selected_ids_without_records,
            selected.difference(&emitted).copied().collect::<Vec<_>>()
        );
    }
}

fn saved_word(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn saved_identifier(mut value: &serde_json::Value) -> Option<u64> {
    loop {
        if let Some(id) = value.as_u64() {
            return Some(id);
        }
        value = value.get("m_id").or_else(|| value.get("m_id64"))?;
    }
}

/// Reference deletion classification from public full JSON plus saved record
/// envelopes, independently of the cached native preamble's projection.
fn raw_deletion_classification(
    file: &mut RevitFile,
    raw: &native_index::ProjectIndex,
) -> (BTreeMap<String, usize>, BTreeMap<String, usize>) {
    use rvt::partition_element_records as records;
    let version = file.basic_file_info().expect("real version").version;
    let marker = records::file_bbox_marker(file, version).expect("qualified record marker");
    let declared = rvt::elem_table::declared_ids(
        &rvt::elem_table::parse_records(file).expect("real declared IDs"),
    );
    let mut grave_counts = BTreeMap::<u64, usize>::new();
    for row in &raw.graveyard_rows {
        let id = saved_identifier(&row["m_id"]).expect("nonnegative saved graveyard ID");
        *grave_counts.entry(id).or_default() += 1;
    }
    let original_ids: BTreeSet<_> = raw
        .identities
        .values()
        .map(|identity| identity.original_id_suffix)
        .collect();
    let mut unresolved = BTreeMap::<String, usize>::new();
    let mut graveyard = BTreeMap::<String, usize>::new();
    let categories: Vec<_> = records::RECOVERED_CATEGORIES
        .iter()
        .map(|(id, _)| *id)
        .collect();
    for stream in file.partition_stream_names() {
        let inflated = file
            .inflated_partition(&stream)
            .expect("inflate real partition");
        let bytes = inflated.bytes();
        let chain = records::partition_record_chain(bytes, &marker);
        let assigned = records::assign_second_prologue_ids(bytes, &marker, &declared);
        let expected =
            records::count_unattributed_frames_excluding(bytes, &categories, &marker, &assigned);
        let mut observed = BTreeMap::<i64, usize>::new();
        for hit in memchr::memmem::find_iter(bytes, &marker) {
            let Some(offset) = hit.checked_sub(records::BBOX_MARKER_OFFSET) else {
                continue;
            };
            if assigned.contains_key(&offset)
                || !saved_word(bytes, offset).is_some_and(records::carries_no_element_id)
                || saved_word(bytes, offset + records::CONTAINER_OFFSET)
                    != Some(records::CONTAINER_NONE)
                || !saved_word(bytes, offset + records::PLACEMENT_KIND_OFFSET)
                    .is_some_and(|kind| kind as u32 == records::PLACEMENT_KIND_INSTANCE)
            {
                continue;
            }
            let Some(span) = records::enclosing_record(&chain, offset) else {
                continue;
            };
            let Some(category) =
                saved_word(bytes, offset + records::CATEGORY_OFFSET).map(|value| value as i64)
            else {
                continue;
            };
            let Some((_, class)) = records::RECOVERED_CATEGORIES
                .iter()
                .find(|(id, _)| *id == category)
            else {
                continue;
            };
            let bounds: Option<Vec<f64>> = (0..6)
                .map(|axis| {
                    saved_word(bytes, offset + records::BBOX_OFFSET + axis * 8).map(f64::from_bits)
                })
                .collect();
            let Some(bounds) = bounds else {
                continue;
            };
            if bounds.iter().any(|value| !value.is_finite())
                || !(0..3)
                    .all(|axis| bounds[axis + 3] - bounds[axis] > records::VOLUME_EPSILON_FEET)
            {
                continue;
            }
            *observed.entry(category).or_default() += 1;
            let proven = offset
                .checked_add(records::BBOX_OFFSET + 48)
                .is_some_and(|end| end <= span.end)
                && grave_counts.get(&span.element_id) == Some(&1)
                && !raw.identities.contains_key(&span.element_id)
                && !original_ids.contains(&span.element_id)
                && !u32::try_from(span.element_id).is_ok_and(|id| declared.contains(&id));
            let counts = if proven {
                &mut graveyard
            } else {
                &mut unresolved
            };
            *counts.entry((*class).into()).or_default() += 1;
        }
        assert_eq!(observed, expected, "raw omission candidate reconciliation");
    }
    (unresolved, graveyard)
}

fn observed_deletion_classification(
    file: &mut RevitFile,
) -> (BTreeMap<String, usize>, BTreeMap<String, usize>) {
    let diagnostics = rvt::ifc::build_export_diagnostics(
        file,
        &rvt::ifc::IfcModel::default(),
        rvt::ifc::ExportDiagnosticsMode::Default,
    );
    assert!(
        !diagnostics
            .warnings
            .iter()
            .any(|warning| warning.starts_with("Could not verify unattributed records")),
        "saved graveyard projection must remain qualified"
    );
    let counts = |reason: &str| {
        diagnostics
            .skipped
            .iter()
            .find(|item| item.reason == reason)
            .map(|item| item.classes.clone())
            .unwrap_or_default()
    };
    (
        counts("element_record_without_element_id"),
        counts("element_record_in_graveyard"),
    )
}

#[test]
fn compact_graveyard_classification_matches_raw_rows_and_narrow_read_fallback() {
    for fixture in project_fixtures::projects() {
        let mut file = RevitFile::open(&fixture.path).expect("open configured project");
        let raw = raw_project_index(&mut file);
        let expected = raw_deletion_classification(&mut file, &raw);
        assert_eq!(observed_deletion_classification(&mut file), expected);
        eprintln!(
            "{:?}: checked {} raw graveyard rows, {} graveyard frames and {} unresolved frames",
            fixture.project,
            raw.graveyard_rows.len(),
            expected.1.values().sum::<usize>(),
            expected.0.values().sum::<usize>()
        );

        if fixture.project == Project::Mechanical {
            // The previous classifier needed no increment stream. Removing it
            // from a copy must not turn valid saved deletion evidence into a
            // refusal merely because the full native preamble is unavailable.
            let mut compound = cfb::CompoundFile::open(std::io::Cursor::new(
                std::fs::read(&fixture.path).expect("read real fallback input"),
            ))
            .expect("open real CFB copy");
            compound
                .remove_stream("Global/DocumentIncrementTable")
                .expect("remove unrelated preamble stream");
            let mut narrow = RevitFile::open_bytes(compound.into_inner().into_inner())
                .expect("open narrow-read copy");
            assert_eq!(observed_deletion_classification(&mut narrow), expected);
        }
    }
}
