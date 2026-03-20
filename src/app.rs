use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
};

use crate::{
    config,
    document::Document,
    error::Result,
    mode::Mode,
    open_candidate::{
        OpenBufferCandidate, OpenCandidate, ProjectFileCandidate, collect_project_file_candidates,
    },
    picker_match,
};

pub struct App {
    pub mode: Mode,
    pub workspace: Workspace,
    pub picker: PickerState,
    pub shell: ShellState,
}

pub struct Workspace {
    pub documents: Vec<DocumentEntry>,
    pub current_index: usize,
}

pub struct DocumentEntry {
    pub path: PathBuf,
    pub document: Document,
}

pub struct PickerState {
    pub query: String,
    pub candidates: Vec<OpenCandidate>,
}

pub struct ShellState {
    pub program: String,
}

impl App {
    pub fn open(path: &Path) -> Result<Self> {
        let document = Document::open(path)?;
        let workspace = Workspace {
            documents: vec![DocumentEntry {
                path: path.to_path_buf(),
                document,
            }],
            current_index: 0,
        };

        let mut app = Self {
            mode: Mode::Normal,
            workspace,
            picker: PickerState {
                query: String::new(),
                candidates: Vec::new(),
            },
            shell: ShellState {
                program: config::shell_program().to_owned(),
            },
        };
        app.refresh_picker_candidates()?;
        Ok(app)
    }

    pub fn render_to<W: Write>(
        &self,
        writer: &mut W,
        page_height: usize,
        page_width: usize,
    ) -> Result<()> {
        self.workspace
            .current_document()
            .render_first_page_to(writer, page_height, page_width)
    }

    pub fn picker_matches(&self) -> Vec<OpenCandidate> {
        picker_match::sort_open_candidates(&self.picker.candidates, &self.picker.query)
    }

    pub fn refresh_picker_candidates(&mut self) -> Result<()> {
        let mut candidates = self.workspace.open_buffer_candidates();
        let open_paths: HashSet<PathBuf> = self
            .workspace
            .documents
            .iter()
            .map(|entry| entry.path.clone())
            .collect();

        for candidate in collect_project_file_candidates()? {
            if open_paths.contains(&candidate.path) {
                continue;
            }
            candidates.push(OpenCandidate::from_project_file(candidate));
        }

        self.picker.candidates = candidates;
        Ok(())
    }
}

impl Workspace {
    pub fn current_document(&self) -> &Document {
        &self.documents[self.current_index].document
    }

    pub fn open_buffer_candidates(&self) -> Vec<OpenCandidate> {
        self.documents
            .iter()
            .map(|entry| {
                OpenCandidate::OpenBuffer(OpenBufferCandidate::new(
                    entry.path.clone(),
                    display_name(&entry.path),
                ))
            })
            .collect()
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

#[allow(dead_code)]
fn _project_file_display_name(candidate: &ProjectFileCandidate) -> &str {
    &candidate.display_name
}
