//! Project files on disk. Media inside the project file's folder is stored
//! by relative path, so a shoot that is synced to another machine (or
//! another OS with a different home directory) opens without editing.
//! Absolute paths that no longer exist are relocated on load by looking for
//! the same tail of the path near the project file.

use crate::model::Project;
use anyhow::{Context, Result};
use std::path::{Component, Path, PathBuf};

pub const EXTENSION: &str = "trio.json";

pub fn save(project: &Project, path: &Path) -> Result<()> {
    let dir = project_dir(path);
    let mut portable = project.clone();
    for p in paths_mut(&mut portable) {
        if let Ok(rel) = p.strip_prefix(&dir) {
            *p = rel.to_path_buf();
        }
    }
    let text = serde_json::to_string_pretty(&portable)?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

pub fn load(path: &Path) -> Result<Project> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut p: Project = serde_json::from_str(&text).context("parsing project")?;
    let dir = project_dir(path);
    for stored in paths_mut(&mut p) {
        *stored = resolve(stored, &dir);
    }
    Ok(p)
}

/// Absolute folder holding the project file.
fn project_dir(path: &Path) -> PathBuf {
    let dir = path.parent().unwrap_or(Path::new(""));
    std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// Every file or folder the project refers to.
fn paths_mut(p: &mut Project) -> Vec<&mut PathBuf> {
    let mut v = Vec::new();
    v.extend(p.wav.as_mut());
    for c in &mut p.cameras {
        v.extend(c.folder.as_mut());
        v.extend(c.clips.iter_mut().map(|clip| &mut clip.path));
    }
    v.extend(p.output.path.as_mut());
    v
}

/// The path as it is on this machine: relative paths hang off the project
/// folder, missing absolute ones are looked for near it. A path that is not
/// found anywhere is returned unchanged so the error names what was stored.
fn resolve(stored: &Path, dir: &Path) -> PathBuf {
    if stored.is_relative() {
        return dir.join(stored);
    }
    if stored.exists() {
        return stored.to_path_buf();
    }
    if let Some(found) = relocate(stored, dir) {
        return found;
    }
    // Output files need not exist yet; relocating the folder is enough.
    if let (Some(parent), Some(name)) = (stored.parent(), stored.file_name()) {
        if let Some(found) = relocate(parent, dir) {
            return found.join(name);
        }
    }
    stored.to_path_buf()
}

/// Looks for the tail of `path` under the project folder or one of its
/// ancestors. The longest tail wins so `Cam1/a.mp4` beats a stray `a.mp4`,
/// and the nearest ancestor wins among equal tails.
fn relocate(path: &Path, dir: &Path) -> Option<PathBuf> {
    let parts: Vec<_> = path
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .collect();
    for n in (1..=parts.len()).rev() {
        let tail: PathBuf = parts[parts.len() - n..].iter().collect();
        for base in dir.ancestors() {
            let candidate = base.join(&tail);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Camera, Clip};

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    fn clip(path: PathBuf) -> Clip {
        Clip {
            path,
            duration: 1.0,
            width: 1280,
            height: 720,
            fps: 30.0,
            rotation: 0,
            hdr: false,
            has_audio: true,
            creation_time: None,
            end_stamped: false,
            offset: 0.0,
            speed: 1.0,
            sync_confidence: None,
        }
    }

    fn shoot(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("trio-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        touch(&dir.join("Cam1/a.mp4"));
        touch(&dir.join("Cam2/b.mov"));
        touch(&dir.join("take.wav"));
        std::fs::create_dir_all(dir.join("exports")).unwrap();
        dir
    }

    fn project_in(root: &Path) -> Project {
        let mut p = Project::default();
        p.wav = Some(root.join("take.wav"));
        p.cameras[0] = Camera {
            folder: Some(root.join("Cam1")),
            clips: vec![clip(root.join("Cam1/a.mp4"))],
            ..Default::default()
        };
        p.cameras[1] = Camera {
            folder: Some(root.join("Cam2")),
            clips: vec![clip(root.join("Cam2/b.mov"))],
            ..Default::default()
        };
        p.output.path = Some(root.join("exports/out.mp4"));
        p
    }

    #[test]
    fn saved_paths_are_relative_to_the_project_file() {
        let dir = shoot("save");
        let file = dir.join("band.trio.json");
        save(&project_in(&dir), &file).unwrap();

        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"Cam1/a.mp4\""), "{text}");
        assert!(text.contains("\"take.wav\""), "{text}");
        assert!(text.contains("\"exports/out.mp4\""), "{text}");
        assert!(
            !text.contains(&dir.to_string_lossy().into_owned()),
            "{text}"
        );

        let loaded = load(&file).unwrap();
        assert_eq!(loaded.wav, Some(dir.join("take.wav")));
        assert_eq!(loaded.cameras[0].folder, Some(dir.join("Cam1")));
        assert_eq!(loaded.cameras[0].clips[0].path, dir.join("Cam1/a.mp4"));
        assert_eq!(loaded.cameras[1].clips[0].path, dir.join("Cam2/b.mov"));
        assert_eq!(loaded.output.path, Some(dir.join("exports/out.mp4")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn paths_from_another_machine_are_found_near_the_project_file() {
        let dir = shoot("relocate");
        let file = dir.join("band.trio.json");
        let elsewhere = Path::new("/nonexistent-home/someone/Sync/shoot");
        let mut p = project_in(elsewhere);
        // A file that was outside the shoot folder and is nowhere on this machine.
        p.cameras[2].folder = Some(PathBuf::from("/nonexistent-home/someone/Other"));
        std::fs::write(&file, serde_json::to_string(&p).unwrap()).unwrap();

        let loaded = load(&file).unwrap();
        assert_eq!(loaded.wav, Some(dir.join("take.wav")));
        assert_eq!(loaded.cameras[0].folder, Some(dir.join("Cam1")));
        assert_eq!(loaded.cameras[0].clips[0].path, dir.join("Cam1/a.mp4"));
        assert_eq!(loaded.cameras[1].clips[0].path, dir.join("Cam2/b.mov"));
        // The export does not exist yet; its folder does.
        assert_eq!(loaded.output.path, Some(dir.join("exports/out.mp4")));
        assert_eq!(
            loaded.cameras[2].folder,
            Some(PathBuf::from("/nonexistent-home/someone/Other"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
