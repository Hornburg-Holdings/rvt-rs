mod common;

use common::project_fixtures::{self, Project};
use common::{sample_for_year, samples_dir};
use serde_json::Value;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn corpus_available() -> bool {
    sample_for_year(2024).exists()
}

fn temp_diagnostics_dir(prefix: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp diagnostics dir");
    dir
}

#[test]
fn saved_graphics_cli_keeps_partial_and_failure_diagnostics() {
    for fixture in project_fixtures::projects() {
        let dir = temp_diagnostics_dir("rvt-rs-saved-graphics");
        let ifc = dir.join("strict.ifc");
        let sidecar = dir.join("strict.json");
        let output = Command::new(env!("CARGO_BIN_EXE_rvt-ifc"))
            .arg(&fixture.path)
            .args(["--mode", "strict", "--saved-meshes", "-o"])
            .arg(&ifc)
            .arg("--diagnostics")
            .arg(&sidecar)
            .output()
            .expect("run strict saved-graphics export");
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("IFC export mode `strict`"));
        assert!(!ifc.exists());
        let diagnostics: rvt::ifc::ExportDiagnostics = serde_json::from_slice(
            &std::fs::read(&sidecar).expect("read saved-graphics diagnostics"),
        )
        .expect("decode saved-graphics diagnostics");
        let report = diagnostics
            .saved_meshes
            .as_ref()
            .expect("saved-graphics report");
        assert!(report.replaced > 0);
        assert!(report.partial_replaced > 0);
        assert!(!report.issues.is_empty());
        assert!(
            diagnostics
                .warnings
                .iter()
                .any(|warning| warning.starts_with("Saved graphics:"))
        );
        assert_eq!(
            diagnostics.confidence.warning_count,
            diagnostics.warnings.len()
        );

        if fixture.project == Project::Structural {
            let mut compound = cfb::CompoundFile::open(std::io::Cursor::new(
                std::fs::read(&fixture.path).expect("read real file for CLI refusal check"),
            ))
            .expect("open real CFB container");
            compound
                .remove_stream("Global/DocumentIncrementTable")
                .expect("remove required saved-graphics stream");
            let damaged = dir.join("missing-increments.rvt");
            std::fs::write(&damaged, compound.into_inner().into_inner())
                .expect("write private damaged copy");
            let output = Command::new(env!("CARGO_BIN_EXE_rvt-ifc"))
                .arg(&damaged)
                .args(["--mode", "geometry", "--saved-meshes", "-o"])
                .arg(&ifc)
                .arg("--diagnostics")
                .arg(&sidecar)
                .output()
                .expect("run saved-graphics refusal export");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(ifc.exists());
            let diagnostics: rvt::ifc::ExportDiagnostics =
                serde_json::from_slice(&std::fs::read(&sidecar).expect("read refusal diagnostics"))
                    .expect("decode refusal diagnostics");
            assert!(diagnostics.saved_meshes.is_none());
            assert_eq!(diagnostics.exported.bounding_box_bodies, report.candidates);
            assert!(
                diagnostics
                    .warnings
                    .iter()
                    .any(|warning| { warning.starts_with("Saved graphics could not be read;") })
            );
            assert_eq!(
                diagnostics.confidence.warning_count,
                diagnostics.warnings.len()
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn rvt_ifc_cli_writes_diagnostics_sidecar() {
    if !corpus_available() {
        eprintln!(
            "skipping rvt-ifc diagnostics CLI assertion: family corpus missing at {}",
            samples_dir().display()
        );
        return;
    }

    let dir = temp_diagnostics_dir("rvt-rs-ifc-diagnostics");
    let ifc_path = dir.join("sample.ifc");
    let diagnostics_path = dir.join("sample.diagnostics.json");

    let output = Command::new(env!("CARGO_BIN_EXE_rvt-ifc"))
        .arg(sample_for_year(2024))
        .arg("-o")
        .arg(&ifc_path)
        .arg("--diagnostics")
        .arg(&diagnostics_path)
        .output()
        .expect("run rvt-ifc");
    assert!(
        output.status.success(),
        "rvt-ifc failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(ifc_path.exists(), "IFC output should exist");
    assert!(
        diagnostics_path.exists(),
        "diagnostics sidecar should exist"
    );
    let json: Value = serde_json::from_slice(
        &std::fs::read(&diagnostics_path).expect("read diagnostics sidecar"),
    )
    .expect("parse diagnostics JSON");
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["mode"], "default");
    assert!(
        json["input"]["has_formats_latest"]
            .as_bool()
            .unwrap_or(false)
    );
    assert!(
        json["input"]["has_global_latest"]
            .as_bool()
            .unwrap_or(false)
    );
    assert!(json["exported"]["total_entities"].as_u64().unwrap_or(0) >= 1);
    assert!(json["confidence"]["level"].is_string());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rvt_ifc_cli_strict_mode_fails_before_writing_ifc_but_keeps_diagnostics() {
    if !corpus_available() {
        eprintln!(
            "skipping rvt-ifc strict mode assertion: family corpus missing at {}",
            samples_dir().display()
        );
        return;
    }

    let dir = temp_diagnostics_dir("rvt-rs-ifc-strict");
    let ifc_path = dir.join("strict.ifc");
    let diagnostics_path = dir.join("strict.diagnostics.json");

    let output = Command::new(env!("CARGO_BIN_EXE_rvt-ifc"))
        .arg(sample_for_year(2024))
        .arg("-o")
        .arg(&ifc_path)
        .arg("--mode")
        .arg("strict")
        .arg("--diagnostics")
        .arg(&diagnostics_path)
        .output()
        .expect("run rvt-ifc strict");

    assert!(
        !output.status.success(),
        "strict mode should reject an incomplete export\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("IFC export mode `strict`"),
        "strict failure should explain the rejected mode\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !ifc_path.exists(),
        "strict mode must not write an IFC that failed the quality gate"
    );
    assert!(
        diagnostics_path.exists(),
        "strict mode should still write requested diagnostics"
    );

    let json: Value = serde_json::from_slice(
        &std::fs::read(&diagnostics_path).expect("read strict diagnostics sidecar"),
    )
    .expect("parse strict diagnostics JSON");
    assert_eq!(json["schema_version"], 1);
    // Strict rejects this export because it carries no element geometry.
    // Which below-geometry level it reports moves as decoders improve:
    // `scaffold` when this test was written, `typed_no_geometry` once the
    // sample family's typed elements began to export. The test pinned the
    // old value for months unseen because no CI job ran this file.
    let level = json["confidence"]["level"].as_str().unwrap_or_default();
    assert!(
        [
            "scaffold",
            "typed_no_geometry",
            "diagnostic_partial",
            "proxy_only"
        ]
        .contains(&level),
        "strict rejected an export at confidence level {level:?}; expected a below-geometry level"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
