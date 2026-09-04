use crate::model::Project;
use anyhow::{Context, Result};
use std::path::Path;

pub const EXTENSION: &str = "trio.json";

pub fn save(project: &Project, path: &Path) -> Result<()> {
    let text = serde_json::to_string_pretty(project)?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

pub fn load(path: &Path) -> Result<Project> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let p: Project = serde_json::from_str(&text).context("parsing project")?;
    Ok(p)
}
