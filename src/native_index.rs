//! Native project identities and creation-episode UniqueIds.
//! Explicit four-field (2023) and five-field (2024 to 2027) ElemTable contracts.
//! Original identity suffixes are retained; they are not current record locators.
use crate::{native_parameters, schema_registry::Registry};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Increment {
    pub greatest_episode: i64,
    pub total_episodes: u64,
    pub stored_elements: u64,
    /// Chronological (causing increment, remaining element count) versions.
    /// Only the terminal zero-count version redirects an empty increment.
    pub redirects: Vec<(i64, u64)>,
    /// Raw sparse episode pairs; direction/history semantics are not inferred.
    /// Nonempty pairs are accepted only under the single-increment routing invariant.
    pub episode_permutation: Vec<(i64, i64)>,
}
/// The increment ordinal is distinct from its greatest modification episode.
pub fn storage_increments(bytes: &[u8], registry: &Registry) -> Result<Vec<Increment>> {
    ensure!(bytes.len() >= 2, "truncated increment class");
    let tag = u16::from_le_bytes(bytes[..2].try_into()?);
    ensure!(
        registry
            .class(tag)
            .is_some_and(|c| c.name == "DocumentIncrementTable"),
        "unsupported increment root class"
    );
    let table = registry
        .class(tag)
        .ok_or_else(|| anyhow::anyhow!("increment root class absent"))?;
    // The measured table consists of three inline-object vectors. Decode and
    // project each row under the default decoder limits, with one aggregate
    // value allowance shared by both histories and the permutation.
    // Unqualified schemas keep the previous complete-object decoder.
    let names = ["m_increments", "m_localIncrements", "m_permutation"];
    if table.parent_reference.tag < 12
        && table.fields.len() == names.len()
        && table.fields.iter().zip(names).all(|(field, name)| {
            field.name == name && field.raw_descriptor == 0x500e && field.references.len() == 1
        })
    {
        let mut pos = 2;
        // Keep the old allowance for small inputs; beyond it, decoding work
        // must be backed by serialized bytes. This also bounds repeated empty
        // inline values, which could otherwise consume little or no input.
        let mut remaining_values = bytes
            .len()
            .max(native_parameters::GraphLimits::default().max_values);
        let mut increments = Vec::new();
        decode_increment_vector(
            bytes,
            registry,
            &table.fields[0],
            &mut pos,
            &mut remaining_values,
            |row| {
                increments.push(decode_increment_row(&row)?);
                Ok(())
            },
        )?;
        validate_increment_ranges(&increments)?;
        let mut local_count = 0;
        decode_increment_vector(
            bytes,
            registry,
            &table.fields[1],
            &mut pos,
            &mut remaining_values,
            |row| {
                let local = decode_increment_row(&row)?;
                ensure!(
                    increments.get(local_count) == Some(&local),
                    "global/local increment routing tables differ"
                );
                local_count += 1;
                Ok(())
            },
        )?;
        ensure!(
            local_count == increments.len(),
            "global/local increment routing tables differ"
        );
        let mut permutation = Vec::new();
        decode_increment_vector(
            bytes,
            registry,
            &table.fields[2],
            &mut pos,
            &mut remaining_values,
            |row| {
                permutation.push(decode_permutation_pair(&row)?);
                Ok(())
            },
        )?;
        qualify_single_increment_permutation(&mut increments, permutation)?;
        ensure!(
            bytes.get(pos..) == Some(&[0; 4]),
            "unsupported increment table terminal"
        );
        return Ok(increments);
    }
    let root = native_parameters::decode_object_fields(bytes, registry, 2, tag)?;
    ensure!(
        bytes.get(root.end..) == Some(&[0; 4]),
        "unsupported increment table terminal"
    );
    let mut increments = reconcile_increment_rows(
        &root.fields["m_increments"],
        &root.fields["m_localIncrements"],
    )?;
    let permutation = root.fields["m_permutation"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("increment permutation absent"))?
        .iter()
        .map(decode_permutation_pair)
        .collect::<Result<Vec<_>>>()?;
    qualify_single_increment_permutation(&mut increments, permutation)?;
    Ok(increments)
}

/// Visit the schema-qualified inline rows without retaining revision metadata.
/// Each row uses the default decoder limits. The caller shares a byte-scaled
/// aggregate value allowance across vectors; every row must consume input.
fn decode_increment_vector(
    bytes: &[u8],
    registry: &Registry,
    field: &crate::schema_registry::Field,
    pos: &mut usize,
    remaining_values: &mut usize,
    mut emit: impl FnMut(serde_json::Value) -> Result<()>,
) -> Result<()> {
    let end = pos
        .checked_add(4)
        .ok_or_else(|| anyhow::anyhow!("increment count offset overflow"))?;
    let count = u32::from_le_bytes(
        bytes
            .get(*pos..end)
            .ok_or_else(|| anyhow::anyhow!("truncated increment vector count"))?
            .try_into()?,
    );
    ensure!(count <= 100_000, "native container count budget exceeded");
    *pos = end;
    for _ in 0..count {
        let (row, used_values) = native_parameters::decode_object_fields_with_value_budget(
            bytes,
            registry,
            *pos,
            field.references[0].tag,
            *remaining_values,
        )?;
        ensure!(row.end > *pos, "increment row consumes no bytes");
        *pos = row.end;
        *remaining_values = remaining_values
            .checked_sub(used_values)
            .ok_or_else(|| anyhow::anyhow!("increment table value budget exceeded"))?;
        emit(row.fields)?;
    }
    Ok(())
}

/// This does not interpret the direction of a sparse episode permutation.
/// With one stored increment and no redirect, all in-range episodes have the
/// same physical route. More general tables remain explicitly unsupported.
fn qualify_single_increment_permutation(
    increments: &mut [Increment],
    pairs: Vec<(i64, i64)>,
) -> Result<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    ensure!(
        increments.len() == 1,
        "multi-increment permutation routing is unsupported"
    );
    let row = &mut increments[0];
    ensure!(
        row.stored_elements > 0 && row.redirects.iter().all(|(target, _)| *target == -1),
        "permuted increment must be stored without redirects"
    );
    let mut keys = BTreeSet::new();
    for &(source, target) in &pairs {
        ensure!(
            source >= 0
                && source <= row.greatest_episode
                && target >= 0
                && target <= row.greatest_episode,
            "permutation endpoint outside sole increment episode range"
        );
        ensure!(keys.insert(source), "duplicate permutation source episode");
    }
    row.episode_permutation = pairs;
    Ok(())
}

fn decode_permutation_pair(pair: &serde_json::Value) -> Result<(i64, i64)> {
    let source = pair["first"]["m_id"]
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("permutation source episode absent"))?;
    let target = pair["second"]
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("permutation target episode absent"))?;
    Ok((source, target))
}

fn reconcile_increment_rows(
    global: &serde_json::Value,
    local: &serde_json::Value,
) -> Result<Vec<Increment>> {
    // Global and local stream bookkeeping revisions can differ after a save.
    // Equality is required for every field that determines physical routing;
    // unrelated stream revision counters are not routing equivalence evidence.
    let increments = decode_increment_rows(global)?;
    let local_increments = decode_increment_rows(local)?;
    ensure!(
        increments == local_increments,
        "global/local increment routing tables differ"
    );
    Ok(increments)
}

fn decode_increment_rows(value: &serde_json::Value) -> Result<Vec<Increment>> {
    let rows = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("increment rows absent"))?;
    let result = rows
        .iter()
        .map(decode_increment_row)
        .collect::<Result<Vec<_>>>()?;
    validate_increment_ranges(&result)?;
    Ok(result)
}

fn validate_increment_ranges(rows: &[Increment]) -> Result<()> {
    let mut previous = -1;
    for row in rows {
        ensure!(
            row.greatest_episode >= previous
                && row.greatest_episode >= -1
                && row.total_episodes >= u64::try_from(row.greatest_episode + 1)?,
            "invalid increment episode range"
        );
        previous = row.greatest_episode;
    }
    Ok(())
}

fn decode_increment_row(row: &serde_json::Value) -> Result<Increment> {
    let greatest_episode = row["m_greatest"]["m_id"]
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("greatest episode absent"))?;
    let total_episodes = row["m_totalEpisodes"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("total episode count absent"))?;
    let stored_elements = row["m_totalElements"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("stored element count absent"))?;
    let mut redirects = Vec::new();
    for r in row["m_incrementVersions"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("increment versions absent"))?
    {
        redirects.push((
            r["m_causedByIncrementNumber"]
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("increment redirect absent"))?,
            r["m_totalElements"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("redirect element count absent"))?,
        ));
    }
    Ok(Increment {
        greatest_episode,
        total_episodes,
        stored_elements,
        redirects,
        episode_permutation: Vec::new(),
    })
}
pub fn route_episode(
    episode: u32,
    increments: &[Increment],
    present: &BTreeSet<u32>,
) -> Result<u32> {
    let initial = increments
        .iter()
        .position(|r| r.greatest_episode >= i64::from(episode))
        .ok_or_else(|| anyhow::anyhow!("modification episode outside increment ranges"))?;
    let mut index = initial;
    for _ in 0..=128 {
        let row = increments
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("increment redirect outside table"))?;
        let mut previous_cause = -2;
        let mut previous_count = u64::MAX;
        for &(cause, count) in &row.redirects {
            ensure!(
                cause >= -1
                    && cause > previous_cause
                    && (cause == -1
                        || (cause as usize > index && (cause as usize) < increments.len()))
                    && count <= previous_count,
                "invalid chronological increment version history"
            );
            previous_cause = cause;
            previous_count = count;
        }
        if let Some(&(_, terminal_count)) = row.redirects.last() {
            ensure!(
                terminal_count == row.stored_elements,
                "increment terminal version count differs from current storage"
            );
        }
        if row.stored_elements > 0 {
            ensure!(
                present.contains(&(index as u32)),
                "active physical increment absent"
            );
            return Ok(index as u32);
        }
        // Earlier nonzero counts describe historical residual populations. They
        // are not current routes. A compaction appends the new increment with
        // count zero; its ordinal is the terminal version's causing increment.
        let &(target, count) = row
            .redirects
            .last()
            .ok_or_else(|| anyhow::anyhow!("unresolved empty increment route"))?;
        ensure!(
            count == 0 && target >= 0 && target as usize > index,
            "unsupported terminal increment redirect relation"
        );
        index = target as usize;
    }
    anyhow::bail!("increment redirect recursion budget exceeded")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub element_id: u64,
    pub original_id_suffix: u64,
    pub creation_episode: u32,
    pub stored_revision: u32,
    pub other_revision: u32,
    pub row_offset: usize,
    pub raw_fields: Vec<u32>,
    pub owning_element_id: i64,
    pub partition_id: i64,
    pub unique_id: String,
}
fn guid_string(b: &[u8; 16]) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u32::from_le_bytes(b[..4].try_into().unwrap()),
        u16::from_le_bytes(b[4..6].try_into().unwrap()),
        u16::from_le_bytes(b[6..8].try_into().unwrap()),
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}
/// Decode the measured post-root episode section. Its start is determined by
/// schema field traversal; it is not a fixed byte offset or byte-pattern scan.
pub fn creation_episodes(history: &[u8], registry: &Registry) -> Result<Vec<String>> {
    ensure!(history.len() >= 2, "truncated history tag");
    let tag = u16::from_le_bytes(history[..2].try_into()?);
    ensure!(
        registry
            .class(tag)
            .is_some_and(|c| c.name == "DocumentHistory"),
        "unsupported history class"
    );
    let root = native_parameters::decode_object_fields(history, registry, 2, tag)?;
    ensure!(
        root.fields["m_episodeList"]["m_oEpisodes"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "inline history episodes require a different contract"
    );
    let count_bytes = history
        .get(root.end..root.end + 4)
        .ok_or_else(|| anyhow::anyhow!("history episode count absent"))?;
    let count = u32::from_le_bytes(count_bytes.try_into()?) as usize;
    ensure!(count <= 1_000_000, "history episode count budget exceeded");
    let episode = registry
        .named("Episode")
        .ok_or_else(|| anyhow::anyhow!("Episode schema absent"))?;
    let mut pos = root.end + 4;
    let mut guids = Vec::new();
    for _ in 0..count {
        let decoded = native_parameters::decode_object_fields(history, registry, pos, episode.tag)?;
        let raw = &decoded.fields["m_eGUID"]["m_guid"]["m_guid"]["guid_bytes"];
        let values = raw
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("unsupported Episode GUID shape"))?;
        ensure!(values.len() == 16, "invalid Episode GUID width");
        let mut bytes = [0u8; 16];
        for (i, v) in values.iter().enumerate() {
            bytes[i] = u8::try_from(
                v.as_u64()
                    .ok_or_else(|| anyhow::anyhow!("invalid GUID byte"))?,
            )?;
        }
        guids.push(guid_string(&bytes));
        pos = decoded.end;
    }
    ensure!(
        history.get(pos..) == Some(&[0; 4]),
        "unsupported history episode trailer"
    );
    Ok(guids)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectIndex {
    pub identities: BTreeMap<u64, Identity>,
    pub graveyard_rows: Vec<serde_json::Value>,
    pub row_stride: usize,
    pub source_fields: serde_json::Value,
}

/// Internal routing and deletion evidence. Every graveyard row is still fully
/// decoded and validated, but its JSON is not retained by the file cache.
pub(crate) struct CompactProjectIndex {
    pub identities: BTreeMap<u64, Identity>,
    pub graveyard_records: usize,
    graveyard_id_counts: BTreeMap<u64, usize>,
    graveyard_identity_diagnostic: Option<String>,
}

impl CompactProjectIndex {
    pub(crate) fn qualified_graveyard_ids(
        &self,
        declared: &BTreeSet<u32>,
    ) -> Result<BTreeSet<u64>> {
        // Raw index parsing does not require a graveyard row to expose a usable
        // ElementId. Only deletion classification needs that projection, and
        // it must keep the previous fail-closed behavior for any invalid row.
        if let Some(diagnostic) = &self.graveyard_identity_diagnostic {
            anyhow::bail!("{diagnostic}");
        }
        let current_original_ids: BTreeSet<_> = self
            .identities
            .values()
            .map(|identity| identity.original_id_suffix)
            .collect();
        Ok(self
            .graveyard_id_counts
            .iter()
            .filter_map(|(&id, &count)| {
                (count == 1
                    && !self.identities.contains_key(&id)
                    && !current_original_ids.contains(&id)
                    && !u32::try_from(id).is_ok_and(|id| declared.contains(&id)))
                .then_some(id)
            })
            .collect())
    }
}

struct ParsedIndex {
    identities: BTreeMap<u64, Identity>,
    graveyard_records: usize,
    row_stride: usize,
    source_fields: serde_json::Value,
}

pub(crate) fn parse_compact(
    index: &[u8],
    registry: &Registry,
    episodes: &[String],
) -> Result<CompactProjectIndex> {
    let mut graveyard_id_counts = BTreeMap::new();
    let mut graveyard_identity_diagnostic = None;
    let parsed = parse_with_graveyard_rows(index, registry, episodes, |row| {
        let id = (|| -> Result<u64> {
            Ok(u64::try_from(crate::native_metadata::identifier(
                &row["m_id"],
            )?)?)
        })();
        match id {
            Ok(id) => *graveyard_id_counts.entry(id).or_insert(0) += 1,
            Err(error) if graveyard_identity_diagnostic.is_none() => {
                graveyard_identity_diagnostic = Some(error.to_string());
            }
            Err(_) => {}
        }
        Ok(())
    })?;
    Ok(CompactProjectIndex {
        identities: parsed.identities,
        graveyard_records: parsed.graveyard_records,
        graveyard_id_counts,
        graveyard_identity_diagnostic,
    })
}
#[derive(Clone)]
struct Leaf {
    path: String,
    offset: usize,
    size: usize,
}
fn fixed_layout(registry: &Registry, tag: u16) -> Result<(usize, Vec<Leaf>)> {
    fn visit(
        registry: &Registry,
        tag: u16,
        prefix: &str,
        pos: &mut usize,
        leaves: &mut Vec<Leaf>,
        depth: usize,
    ) -> Result<()> {
        ensure!(depth < 128, "index layout recursion budget");
        let c = registry
            .class(tag)
            .ok_or_else(|| anyhow::anyhow!("index layout class absent"))?;
        if c.parent_reference.tag >= 12 {
            visit(
                registry,
                c.parent_reference.tag,
                prefix,
                pos,
                leaves,
                depth + 1,
            )?;
        }
        for f in &c.fields {
            if f.uninterpreted_flags & 2 != 0 {
                continue;
            }
            ensure!(
                f.uninterpreted_flags == 0 && f.modifier == 0,
                "nonfixed index schema field {}.{}",
                c.name,
                f.name
            );
            let path = if prefix.is_empty() {
                f.name.clone()
            } else {
                format!("{prefix}.{}", f.name)
            };
            if f.base == 14 {
                ensure!(f.references.len() == 1, "index reference field schema");
                visit(registry, f.references[0].tag, &path, pos, leaves, depth + 1)?;
            } else {
                let size = match f.base {
                    1 | 2 => 1,
                    3 => 2,
                    4..=6 => 4,
                    7 | 11 => 8,
                    9 => 16,
                    _ => anyhow::bail!("unsupported index primitive"),
                };
                leaves.push(Leaf {
                    path,
                    offset: *pos,
                    size,
                });
                *pos += size;
            }
        }
        Ok(())
    }
    let mut size = 0;
    let mut leaves = Vec::new();
    visit(registry, tag, "", &mut size, &mut leaves, 0)?;
    Ok((size, leaves))
}
fn leaf_value(row: &[u8], layout: &[Leaf], prefix: &str) -> Result<(u64, usize)> {
    let candidates: Vec<_> = layout
        .iter()
        .filter(|f| f.path.starts_with(prefix))
        .collect();
    ensure!(
        candidates.len() == 1,
        "ambiguous/missing index field {prefix}"
    );
    let f = candidates[0];
    let bytes = row
        .get(f.offset..f.offset + f.size)
        .ok_or_else(|| anyhow::anyhow!("truncated index field"))?;
    Ok((
        match f.size {
            4 => u64::from(u32::from_le_bytes(bytes.try_into()?)),
            8 => u64::from_le_bytes(bytes.try_into()?),
            _ => anyhow::bail!("unsupported index identity width"),
        },
        f.size,
    ))
}
/// Decode the schema-defined ElemRec array from its true root cursor, followed
/// by graveyard records, flags, deferred IdentifierSource and exact terminator.
/// Original suffixes may repeat; current ElementIds must be unique.
pub fn parse(index: &[u8], registry: &Registry, episodes: &[String]) -> Result<ProjectIndex> {
    let mut graveyard_rows = Vec::new();
    let parsed = parse_with_graveyard_rows(index, registry, episodes, |row| {
        graveyard_rows.push(row);
        Ok(())
    })?;
    Ok(ProjectIndex {
        identities: parsed.identities,
        graveyard_rows,
        row_stride: parsed.row_stride,
        source_fields: parsed.source_fields,
    })
}

/// One parser owns all framing, schema, row and terminal validation. Callers
/// choose only which already-decoded graveyard evidence to retain.
fn parse_with_graveyard_rows(
    index: &[u8],
    registry: &Registry,
    episodes: &[String],
    mut emit_graveyard: impl FnMut(serde_json::Value) -> Result<()>,
) -> Result<ParsedIndex> {
    ensure!(index.len() >= 6, "short project element index");
    let tag = u16::from_le_bytes(index[..2].try_into()?);
    let table = registry
        .class(tag)
        .ok_or_else(|| anyhow::anyhow!("index root absent"))?;
    ensure!(
        table.name == "ElemTable" && matches!(table.fields.len(), 4 | 5),
        "unsupported index root schema"
    );
    let array = &table.fields[0];
    ensure!(
        array.name == "m_elemArr" && array.raw_descriptor == 0x500e && array.references.len() == 1,
        "unsupported index array"
    );
    let (stride, layout) = fixed_layout(registry, array.references[0].tag)?;
    ensure!(stride > 0, "empty index layout");
    let count = u32::from_le_bytes(index[2..6].try_into()?) as usize;
    let end = count
        .checked_mul(stride)
        .and_then(|n| n.checked_add(6))
        .ok_or_else(|| anyhow::anyhow!("index array overflow"))?;
    let rows = index
        .get(6..end)
        .ok_or_else(|| anyhow::anyhow!("truncated index array"))?;
    let mut result = BTreeMap::new();
    for (i, row) in rows.chunks_exact(stride).enumerate() {
        let element_id = leaf_value(row, &layout, "m_id.")?.0;
        let original_id_suffix = leaf_value(row, &layout, "m_history.m_originalElementId.")?.0;
        let creation_episode =
            u32::try_from(leaf_value(row, &layout, "m_history.m_creationDate.")?.0)?;
        let stored_revision =
            u32::try_from(leaf_value(row, &layout, "m_history.m_lastModificationDate.")?.0)?;
        let other_revision =
            u32::try_from(leaf_value(row, &layout, "m_history.m_lastUserModificationDate.")?.0)?;
        let (owner, width) = leaf_value(row, &layout, "m_OwningElementId.")?;
        let owning_element_id = if width == 4 {
            i64::from(owner as u32 as i32)
        } else {
            owner as i64
        };
        let partition_id = i64::from(leaf_value(row, &layout, "m_partitionId.")?.0 as u32 as i32);
        let reverse = episodes
            .len()
            .checked_sub(creation_episode as usize + 1)
            .ok_or_else(|| anyhow::anyhow!("creation episode outside history for {element_id}"))?;
        let unique_id = format!("{}-{original_id_suffix:08x}", episodes[reverse]);
        let raw_fields = row
            .chunks_exact(4)
            .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
            .collect();
        let entry = Identity {
            element_id,
            original_id_suffix,
            creation_episode,
            stored_revision,
            other_revision,
            row_offset: 6 + i * stride,
            raw_fields,
            owning_element_id,
            partition_id,
            unique_id,
        };
        ensure!(
            result.insert(element_id, entry).is_none(),
            "duplicate current ElementId {element_id}"
        );
    }
    let mut pos = end;
    let read_u32 = |pos: &mut usize| -> Result<u32> {
        let b = index
            .get(*pos..*pos + 4)
            .ok_or_else(|| anyhow::anyhow!("truncated index trailer"))?;
        *pos += 4;
        Ok(u32::from_le_bytes(b.try_into()?))
    };
    let grave_count = read_u32(&mut pos)? as usize;
    ensure!(grave_count <= 1_000_000, "graveyard count budget");
    let field = &table.fields[1];
    ensure!(
        field.name == "m_graveyardRecs"
            && field.raw_descriptor == 0x500e
            && field.references.len() == 1,
        "unsupported graveyard schema"
    );
    for _ in 0..grave_count {
        let decoded =
            native_parameters::decode_object_fields(index, registry, pos, field.references[0].tag)?;
        pos = decoded.end;
        emit_graveyard(decoded.fields)?;
    }
    ensure!(
        table.fields[2].name == "m_pSource" && table.fields[2].raw_descriptor == 0x20e,
        "unsupported index source field"
    );
    ensure!(
        read_u32(&mut pos)? == u32::MAX,
        "unsupported identifier source pointer"
    );
    let source_tag = u16::from_le_bytes(
        index
            .get(pos..pos + 2)
            .ok_or_else(|| anyhow::anyhow!("source class absent"))?
            .try_into()?,
    );
    pos += 2;
    // Revit 2023 has only ExpandAllOnLoad. The later contract appends
    // LastElementIdOverride before the deferred source body; consuming it in
    // the older contract would shift m_last and the exact terminal by a byte.
    let flag_names: &[&str] = match table.fields.len() {
        4 => &["m_bExpandAllOnLoad"],
        5 => &["m_bExpandAllOnLoad", "m_bLastElementIdOverride"],
        _ => unreachable!("root field count checked above"),
    };
    for (i, name) in flag_names.iter().enumerate() {
        ensure!(
            table.fields[i + 3].name == *name && table.fields[i + 3].raw_descriptor == 1,
            "unsupported index flag schema"
        );
        ensure!(
            index.get(pos).is_some_and(|v| *v <= 1),
            "invalid index flag"
        );
        pos += 1;
    }
    ensure!(
        registry
            .class(source_tag)
            .is_some_and(|c| c.name == "IdentifierSource"),
        "unsupported identifier source class"
    );
    let source = native_parameters::decode_object_fields(index, registry, pos, source_tag)?;
    pos = source.end;
    ensure!(
        index.get(pos..) == Some(&[0; 4]),
        "unsupported index terminator"
    );
    Ok(ParsedIndex {
        identities: result,
        graveyard_records: grave_count,
        row_stride: stride,
        source_fields: source.fields,
    })
}
