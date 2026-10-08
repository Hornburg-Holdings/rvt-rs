//! Optional project files supplied locally for native-reader regressions.
//! Paths come from the environment; no customer fingerprints or expected model
//! contents are stored in the repository.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Project {
    Structural,
    Mechanical,
}

pub struct Fixture {
    pub path: PathBuf,
    pub project: Project,
}

pub fn projects() -> Vec<Fixture> {
    let mut fixtures = Vec::new();
    for (variable, project) in [
        ("RVT_STRUCTURAL_PROJECT", Project::Structural),
        ("RVT_MECHANICAL_PROJECT", Project::Mechanical),
    ] {
        let Some(path) = std::env::var_os(variable).map(PathBuf::from) else {
            continue;
        };
        assert!(path.is_file(), "configured {variable} is not a file");
        fixtures.push(Fixture { path, project });
    }
    if fixtures.is_empty() {
        eprintln!("skipping: optional native-reader project paths are not set");
    }
    fixtures
}

pub fn project(project: Project) -> Option<PathBuf> {
    projects()
        .into_iter()
        .find(|fixture| fixture.project == project)
        .map(|fixture| fixture.path)
}
