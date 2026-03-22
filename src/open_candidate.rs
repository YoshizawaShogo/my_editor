use std::{
    path::{Path, PathBuf},
    process::Command,
};

use crate::error::{AppError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenCandidate {
    OpenBuffer(OpenBufferCandidate),
    ProjectFile(ProjectFileCandidate),
}

impl OpenCandidate {
    pub fn path(&self) -> &Path {
        match self {
            Self::OpenBuffer(candidate) => &candidate.path,
            Self::ProjectFile(candidate) => &candidate.path,
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            Self::OpenBuffer(candidate) => &candidate.display_name,
            Self::ProjectFile(candidate) => &candidate.display_name,
        }
    }

    pub fn from_project_file(candidate: ProjectFileCandidate) -> Self {
        Self::ProjectFile(candidate)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenBufferCandidate {
    pub path: PathBuf,
    pub display_name: String,
}

impl OpenBufferCandidate {
    pub fn new(path: PathBuf, display_name: String) -> Self {
        Self { path, display_name }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFileCandidate {
    pub path: PathBuf,
    pub display_name: String,
}

impl ProjectFileCandidate {
    pub fn new(path: PathBuf, display_name: String) -> Self {
        Self { path, display_name }
    }
}

pub fn collect_project_file_candidates() -> Result<Vec<ProjectFileCandidate>> {
    let git_root = git_root()?;
    let tracked = git_command_lines(&git_root, &["ls-files"])?;
    let untracked = git_command_lines(&git_root, &["ls-files", "--others", "--exclude-standard"])?;

    let candidates = tracked
        .into_iter()
        .chain(untracked)
        .filter(|line| !line.is_empty())
        .map(|relative_path| {
            ProjectFileCandidate::new(git_root.join(&relative_path), relative_path)
        })
        .collect();

    Ok(candidates)
}

pub fn collect_project_search_paths() -> Result<Vec<PathBuf>> {
    let git_root = git_root()?;
    let tracked = git_command_lines(&git_root, &["ls-files"])?;
    let untracked = git_command_lines(&git_root, &["ls-files", "--others", "--exclude-standard"])?;

    let mut paths = Vec::new();
    for relative_path in tracked.into_iter().chain(untracked.into_iter()) {
        if relative_path.is_empty() {
            continue;
        }
        paths.push(git_root.join(relative_path));
    }

    Ok(paths)
}

pub fn git_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;

    if !output.status.success() {
        return Err(AppError::CommandFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }

    Ok(PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
}

fn git_command_lines(git_root: &Path, args: &[&str]) -> Result<Vec<String>> {
    let output = Command::new("git")
        .current_dir(git_root)
        .args(args)
        .output()?;

    if !output.status.success() {
        return Err(AppError::CommandFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(ToOwned::to_owned)
        .collect())
}
