//! Local phase-level resource evidence; no authored names or values are printed.
use rvt::{
    RevitFile,
    ifc::{ExportDiagnosticsMode, ExportQualityMode, RvtDocExporter, saved_meshes},
};

fn memory(stage: &str) {
    let rss = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmRSS:"))
                .map(str::to_owned)
        });
    eprintln!("{stage}: {}", rss.as_deref().unwrap_or("RSS unavailable"));
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("expected RVT path"))?;
    memory("before_open");
    let mut file = RevitFile::open(path)?;
    memory("after_open");
    let mut model = RvtDocExporter.export_with_mode_and_limits(
        &mut file,
        ExportQualityMode::Geometry,
        Default::default(),
    )?;
    memory("after_model_export");
    let _diagnostics = rvt::ifc::build_export_diagnostics_with_limits(
        &mut file,
        &model,
        ExportDiagnosticsMode::Default,
        Default::default(),
    );
    memory("after_diagnostics");
    memory("after_base_export");
    eprintln!("base_entities={}", model.entities.len());
    let report = saved_meshes::attach(&mut file, &mut model)?;
    memory("after_saved_meshes");
    eprintln!(
        "saved_replaced={}, saved_triangles={}",
        report.replaced, report.triangles
    );
    let step = rvt::ifc::write_step(&model);
    memory("after_step");
    eprintln!("step_bytes={}", step.len());
    Ok(())
}
