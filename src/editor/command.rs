#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    InsertNewline,
    DeleteBackward,
    DeleteForward,
    Move {
        direction: Direction,
        unit: Unit,
        extend: bool,
    },
    SelectAll,
    CollapseSelections,
    AddCursor {
        direction: VerticalDirection,
    },
    SelectNextOccurrence,
    Copy,
    CopyShellSelection,
    Cut,
    Paste,
    Save,
    OpenDirectoryPicker,
    OpenBufferPicker,
    OpenDiffPicker,
    OpenCommandPalette,
    OpenSearch,
    OpenReplace,
    OpenSearchInDirectory,
    CycleSearchScope,
    SearchCursorLeft,
    SearchCursorRight,
    /// Editing the find pane's active field: the same Ctrl bindings the buffer
    /// uses, so the query box behaves like a text field rather than a prompt.
    SearchSelectAll,
    SearchCopy,
    SearchCut,
    SearchPaste,
    SearchUndo,
    SearchRedo,
    SearchToggleField,
    SearchToggleCase,
    SearchToggleWholeWord,
    SearchToggleRegex,
    SearchToggleIgnore,
    SearchToggleHidden,
    ToggleCompletion,
    Rename,
    GoToLine,
    Reload,
    Format,
    ToggleShell,
    ToggleSplit,
    DiffNextHunk,
    DiffPrevHunk,
    CloseBuffer,
    Indent,
    Outdent,
    ToggleComment,
    PickerUp,
    PickerDown,
    PickerBackspace,
    PickerConfirm,
    PickerCancel,
    Cancel,
    Undo,
    Redo,
    NavigateBack,
    NavigateForward,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unit {
    Character,
    Word,
    Line,
    /// Smart Home: the first non-blank column, toggling to column 0 when already
    /// there. Only meaningful with `Direction::Left`.
    LineStartSmart,
    Document,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerticalDirection {
    Up,
    Down,
}
