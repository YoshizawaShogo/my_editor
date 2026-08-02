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
    Document,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerticalDirection {
    Up,
    Down,
}
