use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

// ファイル候補収集時に発生するエラー
#[derive(Debug)]
#[allow(dead_code)]
pub enum Error {
    Io(io::Error),
    GitCommandFailed(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

// ファイルピッカーで表示する候補（開いているバッファ or プロジェクトファイル）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenCandidate {
    OpenBuffer(OpenBufferCandidate),
    ProjectFile(ProjectFileCandidate),
}

impl OpenCandidate {
    // バリアントに関わらず絶対パスを返す
    pub fn path(&self) -> &Path {
        match self {
            Self::OpenBuffer(candidate) => &candidate.path,
            Self::ProjectFile(candidate) => &candidate.path,
        }
    }

    // バリアントに関わらず表示名を返す
    pub fn display_name(&self) -> &str {
        match self {
            Self::OpenBuffer(candidate) => &candidate.display_name,
            Self::ProjectFile(candidate) => &candidate.display_name,
        }
    }

    // ProjectFileCandidate から OpenCandidate へ変換する
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
    // 開いているバッファの候補を生成する
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
    // プロジェクトファイルの候補を生成する
    pub fn new(path: PathBuf, display_name: String) -> Self {
        Self { path, display_name }
    }
}

// git 管理下のファイルを絶対パス付きの候補一覧として返す
pub fn collect_project_file_candidates() -> Result<Vec<ProjectFileCandidate>> {
    let git_root = git_root()?;
    let files = collect_git_files(&git_root)?;
    Ok(files
        .into_iter()
        .map(|relative_path| {
            ProjectFileCandidate::new(git_root.join(&relative_path), relative_path)
        })
        .collect())
}

// プロジェクト内のファイルパス一覧を返す（検索用）
pub fn collect_project_search_paths() -> Result<Vec<PathBuf>> {
    let git_root = git_root()?;
    let files = collect_git_files(&git_root)?;
    Ok(files
        .into_iter()
        .map(|relative_path| git_root.join(relative_path))
        .collect())
}

// tracked と untracked を合わせた相対パス一覧を返す
fn collect_git_files(git_root: &Path) -> Result<Vec<String>> {
    let tracked = git_command_lines(git_root, &["ls-files"])?;
    let untracked = git_command_lines(git_root, &["ls-files", "--others", "--exclude-standard"])?;
    Ok(tracked
        .into_iter()
        .chain(untracked)
        .filter(|line| !line.is_empty())
        .collect())
}

// git rev-parse でリポジトリルートの絶対パスを取得する
fn git_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;

    if !output.status.success() {
        return Err(Error::GitCommandFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }

    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

// git コマンドを実行し stdout を行単位で返す
fn git_command_lines(git_root: &Path, args: &[&str]) -> Result<Vec<String>> {
    let output = Command::new("git")
        .current_dir(git_root)
        .args(args)
        .output()?;

    if !output.status.success() {
        return Err(Error::GitCommandFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(ToOwned::to_owned)
        .collect())
}
