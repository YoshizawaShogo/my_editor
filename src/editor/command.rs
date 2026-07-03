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
    SearchToggleField,
    SearchToggleCase,
    SearchToggleWholeWord,
    SearchToggleRegex,
    SearchToggleIgnore,
    SearchToggleHidden,
    ToggleCompletion,
    Rename,
    Format,
    ToggleShell,
    ToggleSplit,
    CloseBuffer,
    Indent,
    Outdent,
    ToggleComment,
    PickerUp,
    PickerDown,
    PickerBackspace,
    PickerConfirm,
    PickerCancel,
    Undo,
    Redo,
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
