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
    let output = Command::new("git")
        .current_dir(&git_root)
        .args(["ls-files"])
        .output()?;

    if !output.status.success() {
        return Err(AppError::CommandFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let candidates = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|relative_path| {
            ProjectFileCandidate::new(git_root.join(relative_path), relative_path.to_owned())
        })
        .collect();

    Ok(candidates)
}

fn git_root() -> Result<PathBuf> {
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
