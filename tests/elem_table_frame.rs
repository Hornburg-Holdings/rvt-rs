//! `Global/ElemTable` read as the table it is (#152): a `u16` tag, a `u32`
//! record count at `0x02`, and that many records from `0x06`, 28 bytes each
//! through Revit 2023 and 40 from 2024, each closing with its owner
//! (STE1200's frame, measured by puzzbobb on 30 files from 2008 to 2027).
//!
//! What every file of the family corpus (2016 to 2026) and the two MIT
//! projects (Einhoven 2023, Core Interior 2024) must give, from that
//! description alone:
//!
//! 1. `parse_records` returns the count the table states, no more and no
//!    fewer.
//! 2. The records' ids rise strictly: the table is in ascending id order, so
//!    no record is read from the bytes after the last one.
//! 3. Every owner a record names is an id of the same table, no record owns
//!    itself and no owner chain loops.
//! 4. On a family file the first record's owner is element 17, the value
//!    `parse_header` reports as `header_flag`.
//!
//! Corpus resolution follows `tests/elem_table_corpus.rs`: a configured but
//! incomplete corpus fails, an absent one skips and says so.
//!
//! `RVT_ELEM_TABLE_REMAP_DIR` optionally supplies a flat directory of private
//! real `.rvt` files with remapped primary ids and secondary-sorted 40-byte
//! tables. It must contain at least one such file; no file is redistributed.

mod common;

use common::{ALL_YEARS, sample_for_year};
use rvt::{RevitFile, compression, elem_table, streams::GLOBAL_ELEM_TABLE};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn require_family_corpus() -> bool {
    std::env::var("RVT_REQUIRE_CORPUS")
        .ok()
        .is_some_and(|v| v == "1" || v == "true")
}

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

/// The record count the table states, the `u32` at `0x02`.
fn stated_count(path: &Path) -> usize {
    let mut rf = RevitFile::open(path).expect("open");
    let raw = rf.read_stream(GLOBAL_ELEM_TABLE).expect("ElemTable stream");
    let d = compression::inflate_stream_at(GLOBAL_ELEM_TABLE, &raw, 8).expect("inflate");
    u32::from_le_bytes(d[2..6].try_into().expect("four bytes")) as usize
}

/// Checks 1 to 3 on one file; returns the records for file-specific checks.
fn check_table(path: &Path, label: &str) -> Vec<elem_table::ElemRecord> {
    let count = stated_count(path);
    let mut rf = RevitFile::open(path).expect("open");
    let records = elem_table::parse_records(&mut rf).expect("parse_records");

    assert_eq!(
        records.len(),
        count,
        "{label}: parse_records returns the {count} records the table states"
    );
    if let Some(at) = records
        .windows(2)
        .position(|w| w[1].id_primary <= w[0].id_primary)
    {
        panic!(
            "{label}: ids rise strictly, but record {} has id {} after {} (of {})",
            at + 1,
            records[at + 1].id_primary,
            records[at].id_primary,
            records.len()
        );
    }

    let ids: BTreeSet<u32> = records.iter().map(|r| r.id_primary).collect();
    let owners: BTreeMap<u32, u32> = records
        .iter()
        .filter_map(|r| r.owner_id.map(|owner| (r.id_primary, owner)))
        .collect();
    let undeclared: Vec<_> = owners.values().filter(|o| !ids.contains(o)).collect();
    assert!(
        undeclared.is_empty(),
        "{label}: every owner is an id of the table; not declared: {:?}",
        &undeclared[..undeclared.len().min(10)]
    );
    let self_owned: Vec<_> = owners.iter().filter(|(id, owner)| id == owner).collect();
    assert!(
        self_owned.is_empty(),
        "{label}: no record owns itself: {self_owned:?}"
    );
    for &start in owners.keys() {
        let mut at = start;
        for _ in 0..=owners.len() {
            match owners.get(&at) {
                Some(&next) => {
                    assert_ne!(next, start, "{label}: owner chain from {start} loops");
                    at = next;
                }
                None => break,
            }
        }
    }
    records
}

#[test]
fn family_tables_hold_every_stated_record() {
    let mut missing = Vec::new();
    for year in ALL_YEARS {
        let path = sample_for_year(year);
        if !path.exists() {
            missing.push(year);
            continue;
        }
        let label = format!("{year} family");
        let records = check_table(&path, &label);
        assert_eq!(
            records.first().and_then(|r| r.owner_id),
            Some(17),
            "{label}: the first record's owner is element 17"
        );
    }
    assert!(
        missing.is_empty() || !require_family_corpus(),
        "family corpus incomplete, missing {missing:?}, and RVT_REQUIRE_CORPUS is set"
    );
    if !missing.is_empty() {
        eprintln!("skipped family releases (corpus absent): {missing:?}");
    }
}

#[test]
fn project_tables_hold_every_stated_record() {
    let mut read = 0;
    for name in ["Revit_IFC5_Einhoven.rvt", "2024_Core_Interior.rvt"] {
        if let Some(path) = project_file(name) {
            check_table(&path, name);
            read += 1;
        }
    }
    if read == 0 {
        eprintln!("skipping: RVT_PROJECT_CORPUS_DIR is not set");
    }
}

#[test]
fn remapped_tables_keep_the_full_count_and_secondary_order() {
    let Some(dir) = std::env::var_os("RVT_ELEM_TABLE_REMAP_DIR").map(PathBuf::from) else {
        eprintln!("skipping: RVT_ELEM_TABLE_REMAP_DIR is not set");
        return;
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("RVT_ELEM_TABLE_REMAP_DIR {}: {e}", dir.display()))
        .map(|entry| entry.expect("read remapped-table corpus entry").path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("rvt"))
        })
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "RVT_ELEM_TABLE_REMAP_DIR {} contains no real .rvt files",
        dir.display()
    );
    for path in files {
        let label = path.display().to_string();
        let count = stated_count(&path);
        let mut rf = RevitFile::open(&path).expect("open remapped-table file");
        let header = elem_table::parse_header(&mut rf).expect("remapped-table header");
        let layout = elem_table::read_layout(&mut rf).expect("remapped-table layout");
        let records = elem_table::parse_records(&mut rf).expect("remapped-table records");

        assert_eq!(
            header.record_count as usize, count,
            "{label}: full u32 count"
        );
        assert_eq!(layout.start, 0x06, "{label}: record origin");
        assert_eq!(layout.stride, 40, "{label}: explicit record size");
        assert_eq!(
            layout.framing,
            elem_table::RecordFraming::Explicit { marker_len: 8 },
            "{label}: explicit owner field"
        );
        assert_eq!(records.len(), count, "{label}: every stated record is read");
        assert!(
            records
                .windows(2)
                .all(|w| w[0].id_secondary < w[1].id_secondary),
            "{label}: secondary ElementIds rise strictly"
        );
        assert!(
            records.iter().any(|r| r.id_primary != r.id_secondary),
            "{label}: this regression requires remapped primary ids"
        );
        assert!(
            records
                .windows(2)
                .any(|w| w[0].id_primary >= w[1].id_primary),
            "{label}: this regression requires nonascending primary ids"
        );

        let secondary_ids: BTreeSet<u32> = records.iter().map(|r| r.id_secondary).collect();
        assert_eq!(
            secondary_ids.len(),
            count,
            "{label}: secondary ids are unique"
        );
        assert!(
            records
                .iter()
                .filter_map(|r| r.owner_id)
                .all(|owner| secondary_ids.contains(&owner)),
            "{label}: every set owner is declared by a secondary id"
        );
        let declared = elem_table::declared_ids(&records);
        assert!(
            secondary_ids
                .iter()
                .filter(|&&id| id != 0)
                .all(|id| declared.contains(id)),
            "{label}: corrected framing retains declared secondary ElementIds"
        );
    }
}
