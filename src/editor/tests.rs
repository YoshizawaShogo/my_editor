use super::*;
use crate::position::CharIdx;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

#[test]
fn quit_is_a_pure_state_transition_with_an_effect() {
    let mut editor = Editor::default();

    let effects = editor.update(Command::Quit.into());

    assert!(editor.should_quit());
    assert_eq!(effects, vec![Effect::Quit]);
}

#[test]
fn modified_buffer_uses_confirm_before_quit() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextInput('x'));

    assert!(editor.update(Command::Quit.into()).is_empty());
    assert!(!editor.should_quit());
    assert!(editor.confirm_view().is_some());

    assert_eq!(
        editor.update(Command::PickerConfirm.into()),
        vec![Effect::Quit]
    );
    assert!(editor.should_quit());
}

#[test]
fn resize_marks_the_editor_dirty() {
    let mut editor = Editor::default();
    assert!(editor.take_dirty());

    editor.update(AppEvent::Resize {
        cols: 120,
        rows: 40,
    });

    assert_eq!(editor.terminal_size(), (120, 40));
    assert!(editor.take_dirty());
    assert!(!editor.take_dirty());
}

/// Open the shell pane on a fresh session, returning the token the editor
/// asked the runtime to spawn under. Terminal events must carry it to be
/// accepted.
fn open_shell(editor: &mut Editor) -> u64 {
    match editor.update(Command::ToggleShell.into()).as_slice() {
        [Effect::SpawnShell { token, .. }] => *token,
        other => panic!("expected a shell spawn, got {other:?}"),
    }
}

#[test]
fn opening_a_right_pane_replaces_whichever_one_was_there() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });

    editor.update(Command::ToggleShell.into());
    assert!(editor.shell_visible());

    // Ctrl+F over an open shell used to leave both panes alive, with the
    // renderer deciding which one won.
    editor.update(Command::OpenSearch.into());
    assert!(editor.search_pane_visible());
    assert!(!editor.shell_visible());

    editor.update(Command::ToggleSplit.into());
    assert!(editor.split_buffers().is_some());
    assert!(!editor.search_pane_visible());

    editor.update(Command::ToggleShell.into());
    assert!(editor.shell_visible());
    assert!(editor.split_buffers().is_none());
}

#[test]
fn a_replaced_shell_session_cannot_tear_down_its_successor() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });

    let first = open_shell(&mut editor);
    editor.update(AppEvent::Terminal(TerminalEvent::Exited {
        token: first,
        error: None,
    }));
    let second = open_shell(&mut editor);
    assert_ne!(first, second);

    // The dead shell's reader thread reports its exit only now, after the
    // successor is already on screen. Applying it would close the pane and
    // leave the fresh shell with nowhere to draw.
    editor.update(AppEvent::Terminal(TerminalEvent::Exited {
        token: first,
        error: Some("stale".to_owned()),
    }));
    assert!(editor.shell_visible());
    assert_eq!(editor.status(), None);

    editor.update(AppEvent::Terminal(TerminalEvent::Output {
        token: second,
        bytes: b"$ ".to_vec(),
    }));
    assert!(editor.terminal_contents().unwrap().starts_with("$ "));

    editor.update(AppEvent::Terminal(TerminalEvent::Exited {
        token: second,
        error: None,
    }));
    assert!(!editor.shell_visible());
}

#[test]
fn shell_focus_keeps_the_left_editor_available_for_rendering() {
    let mut editor = Editor::default();
    editor.config.editor.shell = Some("/configured/shell".to_owned());
    editor.update(AppEvent::TextPaste("visible".to_owned()));
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });
    editor.hover = Some("old hover".to_owned());

    let effects = editor.update(Command::ToggleShell.into());

    assert!(editor.shell_focused());
    assert!(editor.hover_view().is_none());
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "visible");
    assert!(matches!(
        effects.as_slice(),
        [Effect::SpawnShell { shell, .. }]
            if shell.as_deref() == Some("/configured/shell")
    ));

    let token = match effects.as_slice() {
        [Effect::SpawnShell { token, .. }] => *token,
        other => panic!("expected a shell spawn, got {other:?}"),
    };

    assert!(editor.update(Command::ToggleShell.into()).is_empty());
    assert!(!editor.shell_visible());

    // Reopening resumes the same session, so nothing is spawned.
    assert!(editor.update(Command::ToggleShell.into()).is_empty());
    assert!(editor.shell_visible());
    assert!(editor.shell_focused());

    editor.update(AppEvent::Terminal(TerminalEvent::Exited {
        token,
        error: None,
    }));
    assert!(!editor.shell_visible());
    assert_eq!(editor.status(), None);
}

#[test]
fn hiding_the_shell_pane_keeps_the_session_but_an_exit_ends_it() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });

    let token = open_shell(&mut editor);
    editor.update(AppEvent::Terminal(TerminalEvent::Output {
        token,
        bytes: b"session log".to_vec(),
    }));

    editor.update(Command::ToggleShell.into());
    assert!(!editor.shell_visible());
    // Output that arrives while the pane is hidden still belongs to the
    // session, so it is there when the pane comes back.
    editor.update(AppEvent::Terminal(TerminalEvent::Output {
        token,
        bytes: b" continues".to_vec(),
    }));

    assert!(editor.update(Command::ToggleShell.into()).is_empty());
    assert!(
        editor
            .terminal_contents()
            .unwrap()
            .contains("session log continues")
    );

    // Once the shell exits the session is gone, so the next open spawns.
    editor.update(AppEvent::Terminal(TerminalEvent::Exited {
        token,
        error: None,
    }));
    let next = open_shell(&mut editor);
    assert_ne!(next, token);
    assert_eq!(editor.terminal_contents().unwrap().trim(), "");
}

#[test]
fn shell_drag_selection_copies_a_stable_snapshot_and_clears_hover() {
    let mut editor = Editor::default();
    editor.config.editor.osc52_clipboard = true;
    editor.update(AppEvent::Resize { cols: 20, rows: 6 });
    let token = open_shell(&mut editor);
    editor.update(AppEvent::Terminal(TerminalEvent::Output {
        token,
        bytes: b"hello".to_vec(),
    }));
    editor.hover = Some("old hover".to_owned());

    let mouse = |kind, column| {
        AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind,
                column,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        })
    };
    editor.update(mouse(MouseEventKind::Down(MouseButton::Left), 11));
    editor.update(mouse(MouseEventKind::Drag(MouseButton::Left), 15));
    let effects = editor.update(mouse(MouseEventKind::Up(MouseButton::Left), 15));

    assert_eq!(effects, vec![Effect::ClipboardOsc52("hello".to_owned())]);
    assert_eq!(
        editor.terminal_selection_view(),
        Some(TerminalSelectionView {
            start: (0, 0),
            end: (0, 4),
        })
    );
    assert!(editor.hover.is_none());

    editor.update(AppEvent::Terminal(TerminalEvent::Output {
        token,
        bytes: b"\rXXXXX".to_vec(),
    }));
    assert!(
        editor
            .terminal_screen()
            .unwrap()
            .contents()
            .contains("hello")
    );

    editor.update(AppEvent::TerminalInput(b"x".to_vec()));
    assert!(editor.terminal_selection_view().is_none());
}

#[test]
fn ctrl_c_without_a_shell_selection_still_sends_interrupt() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 20, rows: 6 });
    editor.update(Command::ToggleShell.into());

    assert_eq!(
        editor.update(Command::CopyShellSelection.into()),
        vec![Effect::TerminalInput(vec![3])]
    );
}

#[test]
fn clicking_the_bottom_edge_badge_scrolls_to_the_next_change_below() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 6 });
    let text = (0..30)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    editor.update(AppEvent::TextPaste(text));
    // Bring the viewport back to the top so the change sits off-screen below.
    editor.update(
        Command::Move {
            direction: Direction::Left,
            unit: Unit::Document,
            extend: false,
        }
        .into(),
    );
    let doc = editor.layout.left.view.doc;
    editor
        .documents
        .get_mut(&doc)
        .unwrap()
        .editable_mut()
        .git_lines = vec![GitLine {
        line: 20,
        kind: GitLineKind::Modified,
    }];

    // The bottom badge for a lone "M1" renders "↓ M1" right-aligned in the
    // 40-wide pane, so the "M" segment sits at column 38 on the last text row.
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 38,
            row: 4,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));

    let buffer = editor.active_buffer().unwrap();
    let head = buffer.view.selections.primary().head;
    assert_eq!(buffer.text.char_to_line(head.0), 20);
    assert!(buffer.view.scroll.top_line > 0);
}

#[test]
fn go_to_line_jumps_to_the_typed_line_and_ignores_non_digits() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 10 });
    let text = (0..30)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    editor.update(AppEvent::TextPaste(text));

    editor.update(Command::GoToLine.into());
    assert_eq!(editor.goto_view(), Some(""));
    editor.update(AppEvent::TextInput('x')); // ignored — digits only
    assert_eq!(editor.goto_view(), Some(""));
    editor.update(AppEvent::TextInput('1'));
    editor.update(AppEvent::TextInput('2'));
    assert_eq!(editor.goto_view(), Some("12"));
    editor.update(Command::PickerConfirm.into());

    assert_eq!(editor.goto_view(), None);
    let buffer = editor.active_buffer().unwrap();
    // 1-based line 12 → 0-based line 11.
    assert_eq!(
        buffer
            .text
            .char_to_line(buffer.view.selections.primary().head.0),
        11
    );
}

#[test]
fn go_to_line_escape_leaves_the_caret_where_it_was() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 10 });
    let text = (0..30)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    editor.update(AppEvent::TextPaste(text));
    editor.update(
        Command::Move {
            direction: Direction::Left,
            unit: Unit::Document,
            extend: false,
        }
        .into(),
    );
    let before = editor
        .active_buffer()
        .unwrap()
        .view
        .selections
        .primary()
        .head;

    editor.update(Command::GoToLine.into());
    editor.update(AppEvent::TextInput('5'));
    editor.update(Command::PickerCancel.into());

    assert_eq!(editor.goto_view(), None);
    assert_eq!(
        editor
            .active_buffer()
            .unwrap()
            .view
            .selections
            .primary()
            .head,
        before
    );
}

#[test]
fn go_to_line_clamps_a_number_past_the_end_to_the_last_line() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 10 });
    let text = (0..30)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    editor.update(AppEvent::TextPaste(text));

    editor.update(Command::GoToLine.into());
    for digit in "999".chars() {
        editor.update(AppEvent::TextInput(digit));
    }
    editor.update(Command::PickerConfirm.into());

    let buffer = editor.active_buffer().unwrap();
    assert_eq!(
        buffer
            .text
            .char_to_line(buffer.view.selections.primary().head.0),
        buffer.text.len_lines() - 1
    );
}

#[test]
fn reload_reads_an_unmodified_file_from_disk_immediately() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 10 });
    editor.open_paths([PathBuf::from("main.rs")]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("fn main() {}".to_owned()),
    }));

    let effects = editor.update(Command::Reload.into());
    assert!(matches!(
        effects.as_slice(),
        [Effect::ReadFile { path, .. }] if path == &PathBuf::from("main.rs")
    ));
}

#[test]
fn reload_confirms_before_discarding_unsaved_edits() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 10 });
    editor.open_paths([PathBuf::from("main.rs")]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("fn main() {}".to_owned()),
    }));
    editor.update(AppEvent::TextInput('x'));

    // A modified buffer asks first, and does not read yet.
    let effects = editor.update(Command::Reload.into());
    assert!(effects.is_empty());
    assert!(editor.confirm_view().is_some());

    // Confirming discards the edits and reads from disk.
    let effects = editor.update(Command::PickerConfirm.into());
    assert!(matches!(
        effects.as_slice(),
        [Effect::ReadFile { path, .. }] if path == &PathBuf::from("main.rs")
    ));
}

#[test]
fn event_sequence_edits_moves_and_undoes() {
    let mut editor = Editor::default();

    editor.update(AppEvent::TextInput('a'));
    editor.update(AppEvent::TextInput('b'));
    editor.update(AppEvent::Command(Command::Move {
        direction: Direction::Left,
        unit: Unit::Character,
        extend: false,
    }));
    editor.update(AppEvent::Command(Command::DeleteForward));

    let buffer = editor.active_buffer().unwrap();
    assert_eq!(buffer.text.to_string(), "a");
    assert_eq!(buffer.view.selections.primary().head, CharIdx(1));

    editor.update(AppEvent::Command(Command::Undo));
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "ab");
}

#[test]
fn select_next_occurrence_edits_every_selection() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("one one".to_owned()));

    editor.update(Command::SelectNextOccurrence.into());
    editor.update(Command::SelectNextOccurrence.into());
    editor.update(AppEvent::TextInput('X'));

    let buffer = editor.active_buffer().unwrap();
    assert_eq!(buffer.text.to_string(), "X X");
    assert_eq!(buffer.view.selections.len(), 2);
}

#[test]
fn vertical_cursor_addition_edits_both_lines() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("a\nb".to_owned()));
    editor.update(
        Command::Move {
            direction: Direction::Left,
            unit: Unit::Document,
            extend: false,
        }
        .into(),
    );

    editor.update(
        Command::AddCursor {
            direction: VerticalDirection::Down,
        }
        .into(),
    );
    editor.update(AppEvent::TextInput('X'));

    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "Xa\nXb");
}

#[test]
fn copy_updates_register_and_emits_osc52_effect() {
    let mut editor = Editor::default();
    editor.config.editor.osc52_clipboard = true;
    editor.update(AppEvent::TextPaste("copy me".to_owned()));
    editor.update(Command::SelectAll.into());

    let effects = editor.update(Command::Copy.into());

    assert_eq!(effects, vec![Effect::ClipboardOsc52("copy me".to_owned())]);
}

#[test]
fn copy_without_osc52_enabled_updates_the_register_but_emits_no_effect() {
    // Default config: nothing is written to the terminal (so unsupported
    // terminals don't garble), yet in-editor copy/paste still works.
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("copy me".to_owned()));
    editor.update(Command::SelectAll.into());

    let effects = editor.update(Command::Copy.into());
    assert!(effects.is_empty());

    // The register was still populated: paste re-inserts the text.
    editor.update(Command::CollapseSelections.into());
    editor.update(Command::Paste.into());
    assert_eq!(
        editor.active_buffer().unwrap().text.to_string(),
        "copy mecopy me"
    );
}

#[test]
fn copy_without_a_selection_copies_and_pastes_the_current_line() {
    let mut editor = Editor::default();
    editor.config.editor.osc52_clipboard = true;
    editor.update(AppEvent::TextPaste("one\ntwo".to_owned()));

    let effects = editor.update(Command::Copy.into());

    assert_eq!(effects, vec![Effect::ClipboardOsc52("two\n".to_owned())]);
    editor.update(Command::Paste.into());
    assert_eq!(
        editor.active_buffer().unwrap().text.to_string(),
        "one\ntwo\ntwo"
    );
}

#[test]
fn cut_without_a_selection_cuts_and_restores_the_current_line() {
    // Default config (OSC 52 off): the cut must still delete the caret line —
    // deletion must not depend on the clipboard-push effect being emitted.
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("one\ntwo".to_owned()));
    editor.update(
        Command::Move {
            direction: Direction::Left,
            unit: Unit::Document,
            extend: false,
        }
        .into(),
    );

    let effects = editor.update(Command::Cut.into());

    assert!(effects.is_empty());
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "two");

    editor.update(Command::Undo.into());
    let buffer = editor.active_buffer().unwrap();
    assert_eq!(buffer.text.to_string(), "one\ntwo");
    assert!(buffer.view.selections.primary().is_caret());

    editor.update(Command::Redo.into());
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "two");
    editor.update(Command::Paste.into());
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "one\ntwo");
}

#[test]
fn saving_a_shell_script_requests_a_shellcheck_run() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("script.sh")]);
    let id = DocumentId(1);

    let effects = editor.update(AppEvent::Io(IoEvent::FileSaved { id, result: Ok(()) }));

    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::RunShellcheck { doc, .. } if *doc == id)),
        "saving a .sh file should request a shellcheck run, got {effects:?}"
    );
}

#[test]
fn snippets_appear_as_completions_and_expand_on_confirm() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("x.rs")]);
    // Type "fo"; a manual completion should offer the "for" snippet.
    editor.update(AppEvent::TextInput('f'));
    editor.update(AppEvent::TextInput('o'));
    editor.update(Command::ToggleCompletion.into());

    let view = editor.completion_view().expect("completion popup");
    assert!(
        view.items
            .iter()
            .any(|label| label.contains("for") && label.contains("snippet")),
        "snippet should be offered, got {:?}",
        view.items
    );

    // Confirm the (top-ranked) snippet: it expands and selects the first stop.
    editor.update(Command::PickerConfirm.into());
    let buffer = editor.active_buffer().unwrap();
    assert_eq!(buffer.text.to_string(), "for item in iter {\n    \n}");
    let range = buffer.view.selections.primary().range();
    assert_eq!(buffer.text.slice(range).to_string(), "item");
}

#[test]
fn tab_walks_snippet_stops_and_tracks_edits() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("x.rs")]);
    for character in "for".chars() {
        editor.update(AppEvent::TextInput(character));
    }
    editor.update(Command::ToggleCompletion.into());
    editor.update(Command::PickerConfirm.into());

    // First stop selects "item".
    {
        let buffer = editor.active_buffer().unwrap();
        let range = buffer.view.selections.primary().range();
        assert_eq!(buffer.text.slice(range).to_string(), "item");
    }

    // Overwrite the first placeholder; the later stops must shift with the edit.
    editor.update(AppEvent::TextInput('x'));
    // Tab moves to the second stop, "iter", now three columns earlier.
    editor.update(Command::Indent.into());
    {
        let buffer = editor.active_buffer().unwrap();
        assert!(buffer.text.to_string().starts_with("for x in iter {"));
        let range = buffer.view.selections.primary().range();
        assert_eq!(buffer.text.slice(range).to_string(), "iter");
    }
}

#[test]
fn definition_without_a_language_server_falls_back_to_ctags() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("x.rs")]);
    editor.update(AppEvent::TextPaste("helper".to_owned()));

    // No LSP server is running in the test, so definition takes the ctags path
    // and asks to resolve the symbol under the caret.
    let effects = editor.request_definition();

    assert!(
        matches!(effects.as_slice(), [Effect::CtagsDefinition { symbol, .. }] if symbol == "helper"),
        "expected a ctags lookup for `helper`, got {effects:?}"
    );
}

#[test]
fn ctags_definition_is_not_offered_for_non_target_files() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("notes.md")]);
    editor.update(AppEvent::TextPaste("helper".to_owned()));

    assert!(editor.request_definition().is_empty());
}

#[test]
fn saving_a_csh_file_does_not_request_shellcheck() {
    // csh maps to the "bash" language for highlighting, but shellcheck cannot
    // lint it (SC1071), so no run should be requested.
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("login.csh")]);

    let effects = editor.update(AppEvent::Io(IoEvent::FileSaved {
        id: DocumentId(1),
        result: Ok(()),
    }));

    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::RunShellcheck { .. }))
    );
}

#[test]
fn saving_a_rust_file_does_not_request_shellcheck() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("main.rs")]);

    let effects = editor.update(AppEvent::Io(IoEvent::FileSaved {
        id: DocumentId(1),
        result: Ok(()),
    }));

    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::RunShellcheck { .. }))
    );
}

#[test]
fn drag_selection_auto_scrolls_past_the_bottom_edge() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 20, rows: 6 }); // text rows 0..=4
    let content: String = (0..30).map(|index| format!("line{index}\n")).collect();
    editor.update(AppEvent::TextPaste(content));
    editor.update(
        Command::Move {
            direction: Direction::Left,
            unit: Unit::Document,
            extend: false,
        }
        .into(),
    );
    let top_before = editor.active_buffer().unwrap().view.scroll.top_line;

    let mouse = |kind, row| {
        AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind,
                column: 5,
                row,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        })
    };
    editor.update(mouse(MouseEventKind::Down(MouseButton::Left), 0));
    // Each drag event at the bottom text row (rows - 2) scrolls one line, the
    // way a terminal reports a drag held past the edge.
    for _ in 0..3 {
        editor.update(mouse(MouseEventKind::Drag(MouseButton::Left), 4));
    }

    let buffer = editor.active_buffer().unwrap();
    assert!(
        buffer.view.scroll.top_line > top_before,
        "the view should scroll down as the drag holds at the bottom edge"
    );
    let head_line = buffer
        .text
        .char_to_line(buffer.view.selections.primary().head.0);
    assert!(
        head_line > 4,
        "selection head should extend below the initial viewport, got line {head_line}"
    );
}

#[test]
fn mouse_click_uses_text_coordinates_after_the_gutter() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("abc".to_owned()));
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });

    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));

    assert_eq!(
        editor
            .active_buffer()
            .unwrap()
            .view
            .selections
            .primary()
            .head,
        CharIdx(1)
    );
}

#[test]
fn word_completion_offers_file_words_without_an_lsp() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });
    editor.update(AppEvent::TextPaste("hello helper\nhel".to_owned()));

    editor.update(Command::ToggleCompletion.into());

    let view = editor.completion_view().expect("completion popup");
    assert!(
        view.items.iter().any(|item| item == "hello"),
        "{:?}",
        view.items
    );
    assert!(
        view.items.iter().any(|item| item == "helper"),
        "{:?}",
        view.items
    );
    // The word being typed is not offered as its own completion.
    assert!(!view.items.iter().any(|item| item == "hel"));
}

#[test]
fn typing_a_word_character_pops_word_completion_without_lsp() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });
    editor.update(AppEvent::TextPaste("hello\n".to_owned()));

    editor.update(AppEvent::TextInputAt {
        character: 'h',
        at: std::time::Instant::now(),
    });

    let view = editor.completion_view().expect("completion popup");
    assert!(
        view.items.iter().any(|item| item == "hello"),
        "{:?}",
        view.items
    );
}

#[test]
fn moving_the_caret_dismisses_the_completion_popup() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });
    editor.update(AppEvent::TextPaste("hello\nhel".to_owned()));
    editor.update(Command::ToggleCompletion.into());
    assert!(editor.completion_view().is_some());

    editor.update(
        Command::Move {
            direction: Direction::Left,
            unit: Unit::Character,
            extend: false,
        }
        .into(),
    );

    assert!(editor.completion_view().is_none());
}

#[test]
fn navigation_history_returns_to_the_pre_click_caret() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 10 });
    editor.update(AppEvent::TextPaste("abcdefghij".to_owned()));
    assert_eq!(
        editor.current_location(),
        Some((DocumentId(0), CharIdx(10)))
    );

    // Click into the middle of the line (gutter is 5 wide, so column 8 is
    // display column 3).
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 8,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));
    assert_eq!(editor.current_location(), Some((DocumentId(0), CharIdx(3))));

    editor.update(Command::NavigateBack.into());
    assert_eq!(
        editor.current_location(),
        Some((DocumentId(0), CharIdx(10)))
    );

    editor.update(Command::NavigateForward.into());
    assert_eq!(editor.current_location(), Some((DocumentId(0), CharIdx(3))));
}

#[test]
fn ctrl_click_records_the_pre_click_position_not_the_symbol() {
    // Ctrl+click (go-to-definition) should record where you were, so a single
    // Ctrl+E returns to the reading position rather than the clicked symbol.
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 10 });
    editor.update(AppEvent::TextPaste("first line\nsecond line".to_owned()));
    let origin = editor.current_location(); // end of line 2

    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 8,
            row: 0, // a different line than the caret
            modifiers: KeyModifiers::CONTROL,
        },
        clicks: 1,
    }));
    // The click moved the caret to line 1…
    assert_ne!(editor.current_location(), origin);

    // …and one Back returns straight to the pre-click position.
    editor.update(Command::NavigateBack.into());
    assert_eq!(editor.current_location(), origin);
}

#[test]
fn replace_is_off_until_the_checkbox_is_ticked() {
    let mut editor = Editor::default();
    editor.update(Command::OpenReplace.into());
    assert!(editor.search_view().unwrap().replacement.is_none());

    editor.toggle_replace_field();
    assert!(editor.search_view().unwrap().replacement.is_some());

    editor.toggle_replace_field();
    assert!(editor.search_view().unwrap().replacement.is_none());
}

#[test]
fn clicking_the_gap_between_toggles_flips_the_nearer_one() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 24 });
    editor.update(Command::OpenReplace.into());
    assert!(!editor.search().unwrap().options.case_sensitive);

    // Column 26 is the blank just after the "[Aa]" label; the nearer toggle
    // is still case-sensitivity.
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 26,
            row: 1,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));

    assert!(editor.search().unwrap().options.case_sensitive);
}

#[test]
fn clicking_a_result_opens_it_and_keeps_the_pane_open() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 24 });
    editor.update(AppEvent::TextPaste("foo foo".to_owned()));
    editor.update(Command::OpenReplace.into());
    for character in "foo".chars() {
        editor.update(AppEvent::TextInput(character));
    }
    assert_eq!(editor.search_view().unwrap().total, 2);

    // Results start at row 7 (find box + replace checkbox, then the list border).
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 25,
            row: 7,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));

    // The pane stays open so the remaining hits can be walked one click at a
    // time; only the left editor moves to the match.
    let view = editor.search_view().expect("find pane stays open");
    assert_eq!(view.total, 2);
    assert_eq!(
        editor
            .active_buffer()
            .unwrap()
            .view
            .selections
            .primary()
            .head,
        CharIdx(3)
    );
}

#[test]
fn a_buffer_result_row_marks_the_match_and_dims_the_location_column() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 24 });
    editor.update(AppEvent::TextPaste("    let foo = 1;".to_owned()));
    editor.update(Command::OpenSearch.into());
    for character in "foo".chars() {
        editor.update(AppEvent::TextInput(character));
    }

    let view = editor.search_view().unwrap();
    let item = &view.items[0];
    // The row reads "1 ┆ let foo = 1;" — the leading indent is trimmed, so the
    // highlight has to be shifted by both the prefix and the trimmed whitespace.
    assert_eq!(
        item.text,
        format!("1{}let foo = 1;", SEARCH_COLUMN_SEPARATOR)
    );
    let matched = item.matched.clone().expect("match located");
    let highlighted: String = item
        .text
        .chars()
        .skip(matched.start)
        .take(matched.len())
        .collect();
    assert_eq!(highlighted, "foo");
    // Everything up to and including the separator is the dimmed location column.
    assert_eq!(item.prefix_len, 1 + SEARCH_COLUMN_SEPARATOR.chars().count());
}

/// Open the find pane on a buffer and type `query` into it.
fn find_pane_with(text: &str, query: &str) -> Editor {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 24 });
    editor.update(AppEvent::TextPaste(text.to_owned()));
    editor.update(Command::OpenSearch.into());
    for character in query.chars() {
        editor.update(AppEvent::TextInput(character));
    }
    editor
}

#[test]
fn the_find_field_undoes_and_redoes_edits() {
    let mut editor = find_pane_with("foo", "foo");

    editor.update(Command::SearchUndo.into());
    assert_eq!(editor.search_view().unwrap().query, "fo");
    editor.update(Command::SearchUndo.into());
    assert_eq!(editor.search_view().unwrap().query, "f");

    editor.update(Command::SearchRedo.into());
    assert_eq!(editor.search_view().unwrap().query, "fo");
    editor.update(Command::SearchRedo.into());
    assert_eq!(editor.search_view().unwrap().query, "foo");

    // Nothing left to redo: the command is a no-op rather than a panic.
    editor.update(Command::SearchRedo.into());
    assert_eq!(editor.search_view().unwrap().query, "foo");
}

#[test]
fn the_find_field_selects_all_and_typing_replaces_the_selection() {
    let mut editor = find_pane_with("foo", "foo");

    editor.update(Command::SearchSelectAll.into());
    assert_eq!(editor.search_view().unwrap().field_selection, Some(0..3));

    editor.update(AppEvent::TextInput('x'));
    assert_eq!(editor.search_view().unwrap().query, "x");
    assert_eq!(editor.search_view().unwrap().field_selection, None);

    // One undo restores the whole overwritten value, not one character.
    editor.update(Command::SearchUndo.into());
    assert_eq!(editor.search_view().unwrap().query, "foo");
}

#[test]
fn editing_the_document_leaves_the_results_frozen_until_reload_is_clicked() {
    let mut editor = find_pane_with("foo bar", "foo");
    let before = editor.search_view().unwrap().items[0].text.clone();
    let matched = editor.search_view().unwrap().items[0].matched.clone();

    // Click into the document and type ahead of the match. The stored hit range
    // is frozen, so re-reading the live line here would slide the highlight off.
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));
    for character in "xyz".chars() {
        editor.update(AppEvent::TextInput(character));
    }

    let item = &editor.search_view().unwrap().items[0];
    assert_eq!(item.text, before, "the preview followed the buffer");
    assert_eq!(item.matched, matched, "the highlight drifted");
    let highlighted: String = item
        .text
        .chars()
        .skip(item.matched.clone().unwrap().start)
        .take(item.matched.clone().unwrap().len())
        .collect();
    assert_eq!(highlighted, "foo");

    // Clicking a field only moves the caret — re-running the search there would
    // re-walk a whole repository just because attention returned to the pane.
    let (pane_x, pane_y, pane_width, _) = editor.search_pane_rect();
    let layout = crate::editor::search_pane_layout(false, false);
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane_x + 1,
            row: pane_y + layout.find_top + 1,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));
    assert_eq!(editor.search_view().unwrap().items[0].text, before);

    // The reload button on the results header is what refreshes them.
    let (reload_start, _) = crate::editor::search_reload_button_range(pane_x, pane_width);
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: reload_start,
            row: pane_y + layout.results_top,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));
    assert_eq!(
        editor.search_view().unwrap().items[0].text,
        format!("1{SEARCH_COLUMN_SEPARATOR}xyzfoo bar")
    );
}

#[test]
fn shift_arrows_extend_the_find_field_selection() {
    let mut editor = find_pane_with("foo", "foo");

    // The caret sits at the end; select the last two characters leftwards.
    editor.update(Command::SearchSelectLeft.into());
    editor.update(Command::SearchSelectLeft.into());
    assert_eq!(editor.search_view().unwrap().field_selection, Some(1..3));

    // Shrinking the selection back works too.
    editor.update(Command::SearchSelectRight.into());
    assert_eq!(editor.search_view().unwrap().field_selection, Some(2..3));

    // A plain arrow drops the selection rather than extending it.
    editor.update(Command::SearchCursorLeft.into());
    assert_eq!(editor.search_view().unwrap().field_selection, None);

    // The selection is what Cut takes: the caret is at 1, so this selects and
    // removes the leading "f".
    editor.update(Command::SearchSelectLeft.into());
    assert_eq!(editor.search_view().unwrap().field_selection, Some(0..1));
    editor.update(Command::SearchCut.into());
    assert_eq!(editor.search_view().unwrap().query, "oo");
}

#[test]
fn dragging_in_the_find_field_selects_and_dragging_over_results_does_not() {
    let mut editor = find_pane_with("foo", "foo");
    let (pane_x, pane_y, _, _) = editor.search_pane_rect();
    let layout = crate::editor::search_pane_layout(false, false);
    // The value sits on the middle row of the 3-row Find box.
    let field_row = pane_y + layout.find_top + 1;
    let press = |column: u16, row: u16, kind: MouseEventKind| {
        AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        })
    };

    // Press at the start of the field, drag two characters right.
    editor.update(press(
        pane_x + 1,
        field_row,
        MouseEventKind::Down(MouseButton::Left),
    ));
    assert_eq!(editor.search_view().unwrap().field_selection, None);
    editor.update(press(
        pane_x + 3,
        field_row,
        MouseEventKind::Drag(MouseButton::Left),
    ));
    assert_eq!(editor.search_view().unwrap().field_selection, Some(0..2));

    // A drag over the result list must not drag the caret with it.
    let before = editor.search_view().unwrap().field_selection;
    editor.update(press(
        pane_x + 5,
        pane_y + layout.results_top + 1,
        MouseEventKind::Drag(MouseButton::Left),
    ));
    assert_eq!(editor.search_view().unwrap().field_selection, before);
}

#[test]
fn the_find_field_cuts_copies_and_pastes_through_the_shared_register() {
    let mut editor = find_pane_with("foo", "foo");

    editor.update(Command::SearchSelectAll.into());
    editor.update(Command::SearchCut.into());
    assert_eq!(editor.search_view().unwrap().query, "");

    editor.update(Command::SearchPaste.into());
    assert_eq!(editor.search_view().unwrap().query, "foo");

    // Copy leaves the field alone but refills the register.
    editor.update(Command::SearchSelectAll.into());
    editor.update(Command::SearchCopy.into());
    assert_eq!(editor.search_view().unwrap().query, "foo");
    editor.update(Command::SearchCursorRight.into());
    editor.update(Command::SearchPaste.into());
    assert_eq!(editor.search_view().unwrap().query, "foofoo");
}

#[test]
fn arrow_keys_walk_the_result_list_and_open_each_hit() {
    let mut editor = find_pane_with("foo foo foo", "foo");
    assert_eq!(editor.search_view().unwrap().total, 3);
    assert_eq!(editor.search_view().unwrap().current, None);

    // The first Down lands on the top result rather than skipping one.
    editor.update(Command::PickerDown.into());
    assert_eq!(editor.search_view().unwrap().current, Some(0));
    editor.update(Command::PickerDown.into());
    assert_eq!(editor.search_view().unwrap().current, Some(1));
    editor.update(Command::PickerUp.into());
    assert_eq!(editor.search_view().unwrap().current, Some(0));

    // Stepping past either end stays put instead of wrapping.
    editor.update(Command::PickerUp.into());
    assert_eq!(editor.search_view().unwrap().current, Some(0));

    // Walking the list moves the buffer with it, as clicking does.
    let selection = editor.active_buffer().unwrap().view.selections.primary();
    assert!(!selection.is_caret());
}

#[test]
fn typing_after_opening_a_result_still_edits_the_query() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 24 });
    editor.update(AppEvent::TextPaste("foo foo".to_owned()));
    let before = editor.active_buffer().unwrap().text.to_string();
    editor.update(Command::OpenSearch.into());
    for character in "foo".chars() {
        editor.update(AppEvent::TextInput(character));
    }

    editor.open_search_hit(0);
    editor.update(AppEvent::TextInput('x'));

    // The caret is drawn in the query box, so the keystroke has to land there —
    // it used to go to the document while the caret stayed in the find field.
    assert_eq!(editor.search_view().unwrap().query, "foox");
    assert_eq!(
        editor.active_buffer().unwrap().text.to_string(),
        before,
        "the keystroke leaked into the buffer"
    );
}

/// Click at a terminal cell.
fn click_at(editor: &mut Editor, column: u16, row: u16) {
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));
}

#[test]
fn focus_moves_both_ways_between_the_document_and_the_find_pane() {
    let mut editor = find_pane_with("hello", "hello");
    let (pane_x, pane_y, _, _) = editor.search_pane_rect();
    let layout = crate::editor::search_pane_layout(false, false);
    let field_row = pane_y + layout.find_top + 1;

    // Pane → document.
    click_at(&mut editor, 1, 0);
    assert!(!editor.search_view().unwrap().focused);
    editor.update(AppEvent::TextInput('a'));
    assert_eq!(editor.search_view().unwrap().query, "hello");

    // Document → pane. This is the leg that was missing: the field looked
    // active but keystrokes kept going to the buffer.
    click_at(&mut editor, pane_x + 1, field_row);
    assert!(editor.search_view().unwrap().focused);
    let document = editor.active_buffer().unwrap().text.to_string();
    editor.update(AppEvent::TextInput('b'));
    // The click landed on the field's first column, so the caret is there.
    assert_eq!(editor.search_view().unwrap().query, "bhello");
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), document);

    // And back again, so the round trip is repeatable.
    click_at(&mut editor, 1, 0);
    assert!(!editor.search_view().unwrap().focused);
}

#[test]
fn closing_the_right_pane_never_strands_focus_on_it() {
    // A focus of Overlay or Side::Right outlives its pane if nothing resets it,
    // and a stranded focus swallows every keystroke.
    let mut editor = find_pane_with("hello", "hello");
    assert!(editor.search_view().unwrap().focused);

    // Something that shows one pane on its own while the find pane holds focus.
    let doc = editor.active_buffer().unwrap().view.doc;
    editor.test_show_only(doc);

    assert!(editor.search_view().is_none(), "the pane should be gone");
    editor.update(AppEvent::TextInput('z'));
    assert!(
        editor
            .active_buffer()
            .unwrap()
            .text
            .to_string()
            .contains('z'),
        "the keystroke went nowhere"
    );
}

#[test]
fn clicking_the_document_with_the_find_pane_open_returns_focus_to_it() {
    let mut editor = find_pane_with("hello world", "hello");
    assert!(editor.search_view().unwrap().focused);

    // Click in the document (left half of an 40-column terminal).
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));

    // Focus leaves the pane, so typing edits the document rather than the query.
    assert!(!editor.search_view().unwrap().focused);
    editor.update(AppEvent::TextInput('x'));
    assert_eq!(editor.search_view().unwrap().query, "hello");
    assert!(
        editor
            .active_buffer()
            .unwrap()
            .text
            .to_string()
            .contains('x'),
        "the keystroke never reached the document"
    );
}

#[test]
fn the_find_caret_is_only_drawn_while_the_pane_has_focus() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 24 });
    editor.update(AppEvent::TextPaste("foo".to_owned()));
    editor.update(Command::OpenSearch.into());
    assert!(editor.search_view().unwrap().focused);

    // Focus the document; the pane stays open but must stop claiming the caret.
    editor.focus = Focus::Editor(Side::Left);
    assert!(!editor.search_view().unwrap().focused);
}

#[test]
fn no_result_is_marked_current_until_one_is_opened() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 24 });
    editor.update(AppEvent::TextPaste("foo foo".to_owned()));
    editor.update(Command::OpenSearch.into());
    for character in "foo".chars() {
        editor.update(AppEvent::TextInput(character));
    }

    // Typing a query must not claim the first row is focused.
    assert_eq!(editor.search_view().unwrap().current, None);

    editor.open_search_hit(1);

    // Opening one marks it, so the pane shows which hit the editor sits on.
    assert_eq!(editor.search_view().unwrap().current, Some(1));
}

#[test]
fn result_separators_line_up_across_line_numbers_of_different_widths() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 24 });
    // Hits on line 1 and line 10: a 1-digit and a 2-digit line number.
    let mut text = String::from("foo\n");
    text.push_str(&"x\n".repeat(8));
    text.push_str("foo\n");
    editor.update(AppEvent::TextPaste(text));
    editor.update(Command::OpenSearch.into());
    for character in "foo".chars() {
        editor.update(AppEvent::TextInput(character));
    }

    let view = editor.search_view().unwrap();
    assert_eq!(view.total, 2);
    // The narrower line number is padded, so the separator sits in one column
    // and the match offsets stay correct against the padded prefix.
    let widths: Vec<usize> = view.items.iter().map(|item| item.prefix_len).collect();
    assert_eq!(
        widths[0], widths[1],
        "separator column is ragged: {widths:?}"
    );
    for item in &view.items {
        let matched = item.matched.clone().expect("match located");
        let highlighted: String = item
            .text
            .chars()
            .skip(matched.start)
            .take(matched.len())
            .collect();
        assert_eq!(highlighted, "foo", "row {:?}", item.text);
    }
}

#[test]
fn clicking_the_run_button_replaces_every_match() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 24 });
    editor.update(AppEvent::TextPaste("one two one".to_owned()));
    editor.update(Command::OpenReplace.into());
    for character in "one".chars() {
        editor.update(AppEvent::TextInput(character));
    }
    editor.toggle_replace_field();
    editor.update(AppEvent::TextInput('X'));

    // The run button sits on the checkbox row (row 5), right of "[x] Replace".
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 36,
            row: 5,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));

    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "X two X");
}

#[test]
fn glob_fields_split_into_multiple_patterns_on_whitespace() {
    assert_eq!(
        split_globs("*.rs *.md"),
        vec!["*.rs".to_owned(), "*.md".to_owned()]
    );
    assert_eq!(
        split_globs("   *.rs    *.md   "),
        vec!["*.rs".to_owned(), "*.md".to_owned()]
    );
    assert!(split_globs("").is_empty());
}

#[test]
fn exclude_field_is_empty_with_default_directories_pruned_behind_the_scenes() {
    let mut editor = Editor::default();
    editor.update(Command::OpenReplace.into());

    assert_eq!(editor.search_view().unwrap().exclude, "");
    assert!(
        editor
            .search()
            .unwrap()
            .filters
            .exclude_dirs
            .contains(&".git".to_owned())
    );
}

#[test]
fn search_field_supports_horizontal_cursor_editing() {
    let mut editor = Editor::default();
    editor.update(Command::OpenReplace.into());
    for character in "abc".chars() {
        editor.update(AppEvent::TextInput(character));
    }
    editor.update(Command::SearchCursorLeft.into());
    editor.update(AppEvent::TextInput('X'));

    assert_eq!(editor.search_view().unwrap().query, "abXc");
}

#[test]
fn clicking_a_scope_tab_switches_the_search_scope() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 24 });
    editor.update(Command::OpenReplace.into());
    assert_eq!(editor.search().unwrap().scope, SearchScope::CurrentBuffer);

    // The pane starts at column split_left_width(40)+1 = 21, so its inner
    // content begins at 22 and the " dir " tab spans columns 37..42.
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 38,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));

    assert_eq!(editor.search().unwrap().scope, SearchScope::Directory);
}

#[test]
fn mouse_hit_testing_follows_soft_wrapped_rows() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 10, rows: 8 });
    editor.update(AppEvent::TextPaste("abcdefghij".to_owned()));

    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 1,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));

    assert_eq!(
        editor
            .active_buffer()
            .unwrap()
            .view
            .selections
            .primary()
            .head,
        CharIdx(5)
    );
}

#[test]
fn lsp_hover_is_requested_on_click_not_mouse_move() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("value".to_owned()));
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });
    let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
    document.path = Some(PathBuf::from("/tmp/hover.rs"));
    document.language = Some("rust".to_owned());
    editor.test_register_server("rust", 1).ready = true;
    editor.test_open_doc(DocumentId(0), 1);
    let moved = MouseEvent {
        kind: MouseEventKind::Moved,
        column: 6,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };

    assert!(
        editor
            .update(AppEvent::Mouse(MouseInput {
                event: moved,
                clicks: 0
            }))
            .is_empty()
    );
    assert!(
        !editor
            .pending_lsp
            .values()
            .any(|pending| matches!(pending, PendingLsp::Hover { .. }))
    );

    let effects = editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            ..moved
        },
        clicks: 1,
    }));
    assert!(matches!(
        effects.as_slice(),
        [Effect::LspRequest { method, .. }] if method == "textDocument/hover"
    ));
}

#[test]
fn click_before_lsp_open_is_deferred_and_sent_when_hover_becomes_available() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("value".to_owned()));
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });
    let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
    document.path = Some(PathBuf::from("/tmp/deferred-hover.rs"));
    document.language = Some("rust".to_owned());
    editor.test_register_server("rust", 1);

    let effects = editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));
    assert!(effects.is_empty());
    assert_eq!(editor.deferred_hover, Some((DocumentId(0), CharIdx(1))));

    let effects = editor.update(AppEvent::Lsp(LspEvent::Initialized {
        server: 1,
        incremental_sync: true,
        hover_provider: true,
        semantic_tokens_legend: None,
        signature_help_triggers: Vec::new(),
    }));

    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::LspRequest { id, method, .. }
            if method == "textDocument/hover"
                && matches!(editor.pending_lsp.get(id), Some(PendingLsp::Hover { .. }))
    )));
    assert!(editor.deferred_hover.is_none());
}

#[test]
fn selecting_a_range_dismisses_hover_without_requesting_another() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("value".to_owned()));
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });
    editor.hover = Some("old hover".to_owned());

    let effects = editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 2,
    }));

    assert!(effects.is_empty());
    assert!(editor.hover_view().is_none());
    assert!(
        !editor
            .active_buffer()
            .unwrap()
            .view
            .selections
            .primary()
            .is_caret()
    );
}

#[test]
fn an_empty_hover_response_does_not_open_a_blank_popup() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("value".to_owned()));
    let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
    document.path = Some(PathBuf::from("/tmp/hover.rs"));
    document.language = Some("rust".to_owned());
    editor.test_register_server("rust", 1).ready = true;
    editor.test_open_doc(DocumentId(0), 1);
    editor.pending_lsp.insert(
        42,
        PendingLsp::Hover {
            doc: DocumentId(0),
            line: 0,
        },
    );

    editor.update(AppEvent::Lsp(LspEvent::Response {
        id: 42,
        result: Ok(serde_json::json!({
            "contents": {"kind": "markdown", "value": ""}
        })),
    }));

    assert!(
        editor.hover_view().is_none(),
        "blank hover contents should not open a popup"
    );
}

#[test]
fn edits_preserve_shifted_semantic_colors_and_ignore_old_responses() {
    let mut editor = Editor::default();
    let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
    document.path = Some(PathBuf::from("/tmp/colors.rs"));
    document.language = Some("rust".to_owned());
    document
        .editable_mut()
        .semantic_spans
        .push(crate::lsp::SemanticSpan {
            start: CharIdx(0),
            end: CharIdx(1),
            token_kind: "function".to_owned(),
            token_modifiers: Vec::new(),
        });
    let server = editor.test_register_server("rust", 1);
    server.ready = true;
    server.incremental_sync = true;
    editor.test_open_doc(DocumentId(0), 1);

    let effects = editor.update(AppEvent::TextInput('a'));
    let sync_message = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::LspSend { message, .. } if message.contains("textDocument/didChange") => {
                Some(serde_json::from_str::<serde_json::Value>(message).unwrap())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        sync_message["params"]["contentChanges"][0]["range"]["start"],
        serde_json::json!({"line": 0, "character": 0})
    );
    assert_eq!(sync_message["params"]["contentChanges"][0]["text"], "a");
    let version = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::ScheduleSemanticRefresh { version, .. } => Some(*version),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        editor.active_buffer().unwrap().semantic_spans[0].start,
        CharIdx(1)
    );
    let due = editor.update(AppEvent::Lsp(LspEvent::SemanticRefreshDue {
        doc: DocumentId(0),
        version,
    }));
    let old_request = due
        .iter()
        .find_map(|effect| match effect {
            Effect::LspRequest { id, method, .. }
                if method == "textDocument/semanticTokens/full" =>
            {
                Some(*id)
            }
            _ => None,
        })
        .unwrap();

    editor.update(AppEvent::TextInput('b'));
    editor.update(AppEvent::Lsp(LspEvent::Response {
        id: old_request,
        result: Ok(serde_json::json!({"data": [0, 0, 1, 0, 0]})),
    }));

    let span = &editor.active_buffer().unwrap().semantic_spans[0];
    assert_eq!((span.start, span.end), (CharIdx(2), CharIdx(3)));
}

#[test]
fn semantic_tokens_use_server_legend_names_instead_of_numeric_slots() {
    let mut editor = Editor::default();
    let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
    document.path = Some(PathBuf::from("/tmp/legend.rs"));
    document.language = Some("rust".to_owned());
    document.load_text("fn main() {}");
    editor.test_register_server("rust", 1).semantic_legend =
        Some(crate::lsp::SemanticTokensLegend {
            token_types: vec!["unresolvedReference".to_owned(), "function".to_owned()],
            token_modifiers: vec!["deprecated".to_owned()],
        });

    editor.apply_semantic_tokens(
        DocumentId(0),
        2,
        lsp_types::SemanticTokensResult::Tokens(lsp_types::SemanticTokens {
            result_id: None,
            data: vec![lsp_types::SemanticToken {
                delta_line: 0,
                delta_start: 3,
                length: 4,
                token_type: 1,
                token_modifiers_bitset: 1,
            }],
        }),
    );

    let span = &editor.active_buffer().unwrap().semantic_spans[0];
    assert_eq!(span.token_kind, "function");
    assert_eq!(span.token_modifiers, vec!["deprecated"]);
    assert_eq!((span.start, span.end), (CharIdx(3), CharIdx(7)));
}

#[test]
fn definition_jump_to_unopened_file_lands_on_utf16_position() {
    let mut editor = Editor::default();
    // Stand in for a pending textDocument/definition request.
    editor.pending_lsp.insert(7, PendingLsp::Definition);

    let path = std::path::PathBuf::from("/tmp/def_target.rs");
    editor.update(AppEvent::Lsp(LspEvent::Response {
        id: 7,
        result: Ok(serde_json::json!({
            "uri": format!("file://{}", path.display()),
            "range": {
                "start": {"line": 0, "character": 7},
                "end": {"line": 0, "character": 7},
            },
        })),
    }));

    // The response opens the file asynchronously, so the caret can only be
    // placed once the text arrives. An emoji before the target column makes
    // the UTF-16 column diverge from the char index (char 6, not 7).
    let id = DocumentId(editor.next_doc_id - 1);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id,
        result: Ok("let 😀 = value;".to_owned()),
    }));

    let caret = editor
        .layout
        .active_editor(editor.focus)
        .unwrap()
        .view
        .selections
        .primary()
        .head;
    assert_eq!(caret, CharIdx(6));
    assert!(editor.pending_caret_jumps.is_empty());
}

#[test]
fn definition_jump_scrolls_the_target_line_into_view() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 10 });
    editor.pending_lsp.insert(7, PendingLsp::Definition);

    let path = std::path::PathBuf::from("/tmp/far_target.rs");
    editor.update(AppEvent::Lsp(LspEvent::Response {
        id: 7,
        result: Ok(serde_json::json!({
            "uri": format!("file://{}", path.display()),
            "range": {
                "start": {"line": 30, "character": 0},
                "end": {"line": 30, "character": 0},
            },
        })),
    }));

    let id = DocumentId(editor.next_doc_id - 1);
    let body = (0..40)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id,
        result: Ok(body),
    }));

    // The definition sits on line 30, far below the 9-row text area (10 rows
    // minus the status bar). The view must scroll it into sight AND leave the
    // definition body below it visible. Bottom-pinning would give top_line
    // 30 + 1 - 9 = 22 (target on the last row, nothing below); revealing with
    // context means a larger top_line, so the target sits higher on screen.
    let scroll = editor
        .layout
        .active_editor(editor.focus)
        .unwrap()
        .view
        .scroll
        .top_line;
    assert!(scroll > 0, "expected the view to scroll, got {scroll}");
    assert!(
        scroll > 22,
        "target pinned to the bottom edge with no context below: {scroll}"
    );
    assert!(scroll <= 30);
}

#[test]
fn double_and_triple_click_select_word_and_line() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("one two\nnext".to_owned()));
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });
    let event = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 9,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };

    editor.update(AppEvent::Mouse(MouseInput { event, clicks: 2 }));
    assert_eq!(
        editor
            .active_buffer()
            .unwrap()
            .view
            .selections
            .primary()
            .range(),
        4..7
    );

    editor.update(AppEvent::Mouse(MouseInput { event, clicks: 3 }));
    assert_eq!(
        editor
            .active_buffer()
            .unwrap()
            .view
            .selections
            .primary()
            .range(),
        0..8
    );
}

#[test]
fn diff_picker_opens_current_and_selected_buffers_side_by_side() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("left.txt"), PathBuf::from("right.txt")]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("same\nleft".to_owned()),
    }));
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(2),
        result: Ok("same\nright".to_owned()),
    }));

    editor.update(Command::OpenDiffPicker.into());
    assert_eq!(editor.focus(), Focus::Overlay);
    editor.update(Command::PickerConfirm.into());

    let (left, right, diff) = editor.split_buffers().unwrap();
    assert!(diff);
    assert_eq!(left.text.to_string(), "same\nright");
    assert_eq!(right.text.to_string(), "same\nleft");
}

/// A diff of two files that agree except at rows 2 and 6.
fn diff_editor() -> Editor {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 6 });
    editor.open_paths([PathBuf::from("left.txt"), PathBuf::from("right.txt")]);
    let body =
        |second: &str, sixth: &str| format!("l1\n{second}\nl3\nl4\nl5\n{sixth}\nl7\nl8\nl9\nl10");
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok(body("same", "same")),
    }));
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(2),
        result: Ok(body("CHANGED", "ALSO")),
    }));
    editor.update(Command::OpenDiffPicker.into());
    editor.update(Command::PickerConfirm.into());
    assert!(editor.split_buffers().is_some_and(|(_, _, diff)| diff));
    editor
}

#[test]
fn clicking_in_the_diff_does_not_ask_the_language_server_anything() {
    let mut editor = diff_editor();
    for id in [DocumentId(1), DocumentId(2)] {
        let document = editor.documents.get_mut(&id).unwrap();
        document.path = Some(PathBuf::from(format!("/tmp/diff{}.rs", id.0)));
        document.language = Some("rust".to_owned());
    }
    editor.test_register_server("rust", 1).ready = true;
    editor.test_open_doc(DocumentId(1), 1);
    editor.test_open_doc(DocumentId(2), 1);

    let effects = editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 8,
            row: 1,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));

    // Outside the diff this same click asks for hover; a comparison view
    // must not, which is what made the diff look language-server driven.
    assert!(effects.is_empty());
    assert!(
        !editor
            .pending_lsp
            .values()
            .any(|pending| matches!(pending, PendingLsp::Hover { .. }))
    );
}

#[test]
fn clicking_a_scrolled_diff_row_lands_on_the_line_that_row_shows() {
    let mut editor = diff_editor();
    editor.update(Command::DiffNextHunk.into());
    editor.update(Command::DiffNextHunk.into());
    let top_row = editor.diff_top_row();
    assert!(top_row > 0);

    // Row 0 of the screen is whatever aligned row the view is scrolled to.
    let expected = editor.diff_rows().unwrap()[top_row]
        .left
        .as_ref()
        .unwrap()
        .0;
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 8,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));

    let caret = editor
        .layout
        .active_editor(editor.focus)
        .unwrap()
        .view
        .selections
        .primary()
        .head;
    let text = editor
        .documents
        .get(&DocumentId(2))
        .unwrap()
        .editable()
        .text();
    assert_eq!(text.char_to_line(caret.0), expected);
}

#[test]
fn mouse_wheel_scrolls_the_diff_by_aligned_row() {
    let mut editor = diff_editor();
    assert_eq!(editor.diff_top_row(), 0);

    let wheel = |kind| {
        AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind,
                column: 10,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 0,
        })
    };
    editor.update(wheel(MouseEventKind::ScrollDown));
    assert_eq!(
        editor.diff_top_row(),
        Editor::MOUSE_WHEEL_SCROLL_LINES as usize
    );

    editor.update(wheel(MouseEventKind::ScrollUp));
    assert_eq!(editor.diff_top_row(), 0);

    // Already at the top: scrolling up further must not underflow.
    editor.update(wheel(MouseEventKind::ScrollUp));
    assert_eq!(editor.diff_top_row(), 0);
}

#[test]
fn mouse_wheel_scrolls_the_pane_under_the_cursor_not_the_active_one() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 10 });
    let text = (0..40)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    editor.update(AppEvent::TextPaste(text));
    editor.update(
        Command::Move {
            direction: Direction::Left,
            unit: Unit::Document,
            extend: false,
        }
        .into(),
    );
    // Both halves show the same document, each starting at the top; the right
    // half is the active one.
    editor.update(Command::ToggleSplit.into());
    assert_eq!(editor.focus, Focus::Editor(Side::Right));

    // Spin the wheel over the left (inactive) half at column 10.
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 2,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 0,
    }));

    assert_eq!(
        editor.layout.left.view.scroll.top_line,
        Editor::MOUSE_WHEEL_SCROLL_LINES as usize
    );
    assert_eq!(
        editor.layout.right_editor().unwrap().view.scroll.top_line,
        0
    );
    // Scrolling an inactive pane must not steal focus.
    assert_eq!(editor.focus, Focus::Editor(Side::Right));
}

#[test]
fn diff_navigation_steps_between_hunks_and_stops_at_the_ends() {
    let mut editor = diff_editor();
    let (_, total) = editor.diff_hunk_position().unwrap();
    assert_eq!(total, 2);

    editor.update(Command::DiffNextHunk.into());
    let first = editor.diff_top_row();
    assert!(first > 0);
    assert_eq!(editor.diff_hunk_position().unwrap(), (1, 2));

    editor.update(Command::DiffNextHunk.into());
    assert!(editor.diff_top_row() > first);
    assert_eq!(editor.diff_hunk_position().unwrap(), (2, 2));

    // Past the last hunk the view stays put rather than wrapping.
    let last = editor.diff_top_row();
    editor.update(Command::DiffNextHunk.into());
    assert_eq!(editor.diff_top_row(), last);

    editor.update(Command::DiffPrevHunk.into());
    assert_eq!(editor.diff_top_row(), first);
}

#[test]
fn clicking_the_diff_navigator_arrows_moves_between_hunks() {
    let mut editor = diff_editor();
    let (current, total) = editor.diff_hunk_position().unwrap();
    let pane_x = split_left_width(80).saturating_add(1);
    let width = 80 - pane_x;
    let label_width = crate::render::diff_navigator_label_width(current, total);
    // The arrows sit at the right edge of the diff pane, on its first row.
    let next_column = pane_x + width - label_width + 4;

    let click = |column| {
        AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        })
    };
    editor.update(click(next_column));
    assert_eq!(editor.diff_hunk_position().unwrap().0, 1);
    editor.update(click(next_column));
    assert_eq!(editor.diff_hunk_position().unwrap().0, 2);

    editor.update(click(next_column - 3));
    assert_eq!(editor.diff_hunk_position().unwrap().0, 1);
}

#[test]
fn mouse_click_switches_the_active_split_buffer() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });
    editor.open_paths([PathBuf::from("left.txt"), PathBuf::from("right.txt")]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("left".to_owned()),
    }));
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(2),
        result: Ok("right".to_owned()),
    }));
    editor.update(Command::OpenDiffPicker.into());
    editor.update(Command::PickerConfirm.into());

    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 60,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));
    assert_eq!(editor.focus, Focus::Editor(Side::Right));
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "left");
    editor.update(AppEvent::TextInput('X'));
    let (left, right, _) = editor.split_buffers().unwrap();
    assert_eq!(left.text.to_string(), "right");
    assert_eq!(right.text.to_string(), "leftX");

    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));
    assert_eq!(editor.focus, Focus::Editor(Side::Left));
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "right");
}

/// Open two buffers and start the diff picker, returning the scan token.
fn diff_picker(editor: &mut Editor) -> u64 {
    editor.open_paths([PathBuf::from("left.txt"), PathBuf::from("right.txt")]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("left".to_owned()),
    }));
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(2),
        result: Ok("right".to_owned()),
    }));
    editor
        .update(Command::OpenDiffPicker.into())
        .into_iter()
        .find_map(|effect| match effect {
            Effect::StartFileScan { token, .. } => Some(token),
            _ => None,
        })
        .expect("the diff picker scans for unopened files")
}

#[test]
fn diff_picker_offers_unopened_files_below_the_open_buffers() {
    let mut editor = Editor::default();
    let token = diff_picker(&mut editor);
    assert_eq!(editor.picker_view().unwrap().total, 1);

    editor.update(AppEvent::FileScan(FileScanEvent::Batch {
        token,
        // left.txt is already a buffer, so only the unopened file is added.
        paths: vec![PathBuf::from("left.txt"), PathBuf::from("unopened.rs")],
    }));

    let labels: Vec<_> = editor
        .picker_view()
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert_eq!(
        labels,
        vec!["left.txt".to_owned(), "unopened.rs".to_owned()]
    );
}

#[test]
fn file_picker_lists_files_that_are_already_open() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("open.rs")]);
    let token = editor
        .open_directory_picker()
        .into_iter()
        .find_map(|effect| match effect {
            Effect::StartFileScan { token, .. } => Some(token),
            _ => None,
        })
        .expect("the file picker scans the workspace");

    editor.update(AppEvent::FileScan(FileScanEvent::Batch {
        token,
        paths: vec![PathBuf::from("open.rs"), PathBuf::from("other.rs")],
    }));

    let labels: Vec<_> = editor
        .picker_view()
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.label)
        .collect();
    // The already-open file is not hidden — it shows alongside the rest.
    assert_eq!(labels, vec!["open.rs".to_owned(), "other.rs".to_owned()]);
}

#[test]
fn diffing_an_unopened_file_loads_it_into_the_right_pane() {
    let mut editor = Editor::default();
    let token = diff_picker(&mut editor);
    editor.update(AppEvent::FileScan(FileScanEvent::Batch {
        token,
        paths: vec![PathBuf::from("unopened.rs")],
    }));
    for character in "unopened".chars() {
        editor.update(AppEvent::TextInput(character));
    }

    let effects = editor.update(Command::PickerConfirm.into());

    assert!(
        matches!(effects.as_slice(), [Effect::ReadFile { path, .. }] if path == &PathBuf::from("unopened.rs"))
    );
    let (left, _, diff) = editor.split_buffers().expect("a diff is open");
    assert!(diff);
    // The buffer we came from stays on the left; the picked file loads right.
    assert_eq!(left.text.to_string(), "right");
}

#[test]
fn only_committed_path_prefixes_switch_the_picker_to_a_listing() {
    for query in ["/etc", "~/src", "./src", "..", "../up"] {
        assert!(is_path_query(query), "{query} should list a directory");
    }
    // Still useful as fuzzy patterns, so they must keep matching candidates.
    for query in [".rs", "main.rs", "", "src"] {
        assert!(!is_path_query(query), "{query} should stay a fuzzy pattern");
    }
}

#[test]
fn an_absolute_query_lists_the_directory_instead_of_matching_nothing() {
    let mut editor = Editor::default();
    let token = diff_picker(&mut editor);
    editor.update(AppEvent::FileScan(FileScanEvent::Batch {
        token,
        paths: vec![PathBuf::from("unopened.rs")],
    }));

    let mut effects = Vec::new();
    for character in "/us".chars() {
        effects = editor.update(AppEvent::TextInput(character));
    }
    let listing_token = match effects.as_slice() {
        [Effect::ListPathCompletions { input, token, .. }] if input == "/us" => *token,
        other => panic!("expected a directory listing, got {other:?}"),
    };

    editor.update(AppEvent::FileScan(FileScanEvent::PathCompletions {
        token: listing_token,
        paths: vec![PathBuf::from("/usr"), PathBuf::from("/usr/share")],
    }));
    let labels: Vec<_> = editor
        .picker_view()
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert_eq!(labels, vec!["/usr".to_owned(), "/usr/share".to_owned()]);

    // Deleting back to a fuzzy query restores the picker's own candidates.
    for _ in 0.."/us".len() {
        editor.update(Command::PickerBackspace.into());
    }
    let labels: Vec<_> = editor
        .picker_view()
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert_eq!(
        labels,
        vec!["left.txt".to_owned(), "unopened.rs".to_owned()]
    );
}

#[test]
fn toggle_split_takes_the_diffs_half_over_instead_of_just_dismissing_it() {
    let mut editor = Editor::default();
    let token = diff_picker(&mut editor);
    editor.update(AppEvent::FileScan(FileScanEvent::Done { token }));
    editor.update(Command::PickerConfirm.into());
    assert!(editor.split_buffers().is_some_and(|(_, _, diff)| diff));

    editor.update(Command::ToggleSplit.into());

    // One press lands on a plain split rather than closing, so it no longer
    // takes two presses to get there.
    let (_, _, diff) = editor.split_buffers().expect("a split is open");
    assert!(!diff);
    assert_eq!(editor.focus(), Focus::Editor(Side::Right));

    // And it still toggles its own pane closed.
    editor.update(Command::ToggleSplit.into());
    assert!(editor.split_buffers().is_none());
}

#[test]
fn esc_and_f6_each_close_the_diff() {
    for close in [Command::CollapseSelections, Command::OpenDiffPicker] {
        let mut editor = Editor::default();
        let token = diff_picker(&mut editor);
        editor.update(AppEvent::FileScan(FileScanEvent::Done { token }));
        editor.update(Command::PickerConfirm.into());
        assert!(editor.split_buffers().is_some_and(|(_, _, diff)| diff));

        assert!(editor.update(close.into()).is_empty());

        assert!(
            editor.split_buffers().is_none(),
            "{close:?} left the diff open"
        );
        assert!(editor.picker_view().is_none(), "{close:?} opened a picker");
        assert_eq!(editor.focus(), Focus::Editor(Side::Left));
    }
}

#[test]
fn esc_still_collapses_selections_when_no_diff_is_open() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("hello".to_owned()));
    editor.update(Command::SelectAll.into());
    assert!(
        editor
            .layout
            .left
            .view
            .selections
            .iter()
            .any(|selection| !selection.is_caret())
    );

    editor.update(Command::CollapseSelections.into());

    assert!(
        editor
            .layout
            .left
            .view
            .selections
            .iter()
            .all(|selection| selection.is_caret())
    );
}

#[test]
fn opening_a_file_from_picker_exits_diff_mode() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("left.txt"), PathBuf::from("right.txt")]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("left".to_owned()),
    }));
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(2),
        result: Ok("right".to_owned()),
    }));
    editor.update(Command::OpenDiffPicker.into());
    editor.update(Command::PickerConfirm.into());
    assert!(editor.split_buffers().is_some_and(|(_, _, diff)| diff));

    let token = editor
        .update(Command::OpenDirectoryPicker.into())
        .into_iter()
        .find_map(|effect| match effect {
            Effect::StartFileScan { token, .. } => Some(token),
            _ => None,
        })
        .expect("the directory picker starts a scan");
    editor.update(AppEvent::FileScan(FileScanEvent::Batch {
        token,
        paths: vec![PathBuf::from("next.txt")],
    }));
    let effects = editor.update(Command::PickerConfirm.into());

    assert!(editor.split_buffers().is_none());
    assert!(
        matches!(effects.as_slice(), [Effect::ReadFile { path, .. }] if path == &PathBuf::from("next.txt"))
    );
}

#[test]
fn buffer_picker_replaces_the_mouse_focused_pane_without_closing_split() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });
    editor.open_paths([PathBuf::from("left.txt"), PathBuf::from("right.txt")]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("left".to_owned()),
    }));
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(2),
        result: Ok("right".to_owned()),
    }));
    editor.update(Command::ToggleSplit.into());
    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));
    editor.update(Command::OpenBufferPicker.into());
    for character in "left.txt".chars() {
        editor.update(AppEvent::TextInput(character));
    }
    editor.update(Command::PickerConfirm.into());

    let (left, right, diff) = editor.split_buffers().unwrap();
    assert!(!diff);
    assert_eq!(left.text.to_string(), "left");
    assert_eq!(right.text.to_string(), "right");
    assert_eq!(editor.focus, Focus::Editor(Side::Left));
}

#[test]
fn buffer_picker_excludes_the_current_buffer() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("other.txt"), PathBuf::from("current.txt")]);

    editor.update(Command::OpenBufferPicker.into());

    let picker = editor.picker_view().unwrap();
    assert_eq!(picker.items.len(), 1);
    assert!(picker.items[0].label.contains("other.txt"));
    assert!(!picker.items[0].label.contains("current.txt"));
}

#[test]
fn diff_picker_excludes_another_document_for_the_same_file() {
    let mut editor = Editor::default();
    editor.open_paths([
        PathBuf::from("same.txt"),
        PathBuf::from("other.txt"),
        PathBuf::from("same.txt"),
    ]);

    editor.update(Command::OpenDiffPicker.into());

    let picker = editor.picker_view().unwrap();
    assert_eq!(picker.items.len(), 1);
    assert!(picker.items[0].label.contains("other.txt"));
    assert!(!picker.items[0].label.contains("same.txt"));
}

#[test]
fn command_palette_searches_labels_and_executes_selected_command() {
    let mut editor = Editor::default();
    editor.update(Command::OpenCommandPalette.into());
    for character in "find file".chars() {
        editor.update(AppEvent::TextInput(character));
    }

    let palette = editor.picker_view().unwrap();
    assert!(palette.items[0].label.contains("Ctrl+T"));
    assert!(palette.items[0].label.contains("Find File"));

    let effects = editor.update(Command::PickerConfirm.into());
    assert!(matches!(effects.as_slice(), [Effect::StartFileScan { .. }]));
}

#[test]
fn command_palette_exposes_diff_keybinding() {
    let mut editor = Editor::default();
    editor.update(Command::OpenCommandPalette.into());
    for character in "diff".chars() {
        editor.update(AppEvent::TextInput(character));
    }

    let palette = editor.picker_view().unwrap();
    // Several entries mention diff now, so match on content rather than rank.
    assert!(
        palette
            .items
            .iter()
            .any(|item| item.label.contains("F6") && item.label.contains("Diff"))
    );
}

#[test]
fn opening_any_picker_dismisses_hover() {
    let mut editor = Editor::default();
    for command in [
        Command::OpenCommandPalette,
        Command::OpenDirectoryPicker,
        Command::OpenBufferPicker,
        Command::OpenDiffPicker,
    ] {
        editor.hover = Some("hover text".to_owned());
        editor.update(command.into());
        assert!(editor.hover_view().is_none());
        editor.update(Command::PickerCancel.into());
    }
}

#[test]
fn directory_picker_replaces_the_find_pane_and_receives_keyboard_input() {
    let mut editor = Editor::default();
    editor.update(Command::OpenSearch.into());
    assert!(editor.search_view().is_some());

    let effects = editor.update(Command::OpenDirectoryPicker.into());
    editor.update(AppEvent::TextInput('m'));

    assert!(matches!(effects.as_slice(), [Effect::StartFileScan { .. }]));
    assert!(editor.search_view().is_none());
    assert_eq!(editor.picker_view().unwrap().query, "m");
}

#[test]
fn ctrl_t_and_ctrl_g_toggle_their_picker_closed() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });

    assert!(matches!(
        editor
            .update(Command::OpenDirectoryPicker.into())
            .as_slice(),
        [Effect::StartFileScan { .. }]
    ));
    assert!(editor.picker_view().is_some());
    assert!(
        editor
            .update(Command::OpenDirectoryPicker.into())
            .is_empty()
    );
    assert!(editor.picker_view().is_none());

    editor.open_paths([PathBuf::from("one.txt"), PathBuf::from("two.txt")]);
    editor.update(Command::OpenBufferPicker.into());
    assert!(editor.picker_view().is_some());
    editor.update(Command::OpenBufferPicker.into());
    assert!(editor.picker_view().is_none());
}

#[test]
fn clicking_outside_the_picker_closes_it() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });
    editor.update(Command::OpenCommandPalette.into());

    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));

    assert!(editor.picker_view().is_none());
    assert_eq!(editor.focus(), Focus::Editor(Side::Left));
}

#[test]
fn stale_file_scan_batches_do_not_enter_a_reopened_picker() {
    let mut editor = Editor::default();
    let first = editor
        .update(Command::OpenDirectoryPicker.into())
        .into_iter()
        .find_map(|effect| match effect {
            Effect::StartFileScan { token, .. } => Some(token),
            _ => None,
        })
        .unwrap();
    editor.update(Command::OpenDirectoryPicker.into());
    let second = editor
        .update(Command::OpenDirectoryPicker.into())
        .into_iter()
        .find_map(|effect| match effect {
            Effect::StartFileScan { token, .. } => Some(token),
            _ => None,
        })
        .unwrap();

    editor.update(AppEvent::FileScan(FileScanEvent::Batch {
        token: first,
        paths: vec![PathBuf::from("stale.txt")],
    }));
    assert_eq!(editor.picker_view().unwrap().total, 0);

    editor.update(AppEvent::FileScan(FileScanEvent::Batch {
        token: second,
        paths: vec![PathBuf::from("current.txt")],
    }));
    assert_eq!(editor.picker_view().unwrap().total, 1);
}

#[test]
fn command_palette_restores_the_originating_pane_focus() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("abc".to_owned()));
    editor.update(Command::ToggleSplit.into());
    assert_eq!(editor.focus, Focus::Editor(Side::Right));
    editor.update(Command::OpenCommandPalette.into());
    for character in "select all".chars() {
        editor.update(AppEvent::TextInput(character));
    }

    editor.update(Command::PickerConfirm.into());

    assert_eq!(editor.focus, Focus::Editor(Side::Right));
    assert_eq!(
        editor
            .active_buffer()
            .unwrap()
            .view
            .selections
            .primary()
            .range(),
        0..3
    );
}

#[test]
fn picker_view_virtualizes_large_candidate_lists() {
    let mut editor = Editor::default();
    editor.open_directory_picker();
    let picker = editor.picker.as_mut().unwrap();
    picker.candidates = (0..10_000)
        .map(|index| PickerCandidate::Path(PathBuf::from(format!("file-{index}.txt"))))
        .collect();
    picker.filtered = (0..picker.candidates.len()).collect();
    picker.selected = 9_000;

    let view = editor.picker_view().unwrap();

    assert_eq!(view.items.len(), PICKER_VIEW_WINDOW);
    assert!(view.selected < view.items.len());
    assert!(view.items[view.selected].label.contains("file-9000.txt"));
    assert!(view.has_before);
    assert!(view.has_after);
}

#[test]
fn picker_ignores_mouse_input() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 80, rows: 24 });
    editor.open_command_palette();
    let selected = editor.picker.as_ref().unwrap().selected;

    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 1,
    }));

    // The picker is keyboard-only; the click neither closes it nor moves it.
    assert!(editor.picker.is_some());
    assert_eq!(editor.picker.as_ref().unwrap().selected, selected);
}

#[test]
fn opening_the_find_pane_dismisses_the_hover_popup() {
    let mut editor = Editor {
        hover: Some("docs".to_owned()),
        ..Default::default()
    };

    editor.update(Command::OpenReplace.into());

    assert!(editor.hover_view().is_none());
}

#[test]
fn picker_backspace_restores_cached_prefix_ranking() {
    let mut editor = Editor::default();
    editor.open_directory_picker();
    let picker = editor.picker.as_mut().unwrap();
    picker.candidates = (0..10_000)
        .map(|index| PickerCandidate::Path(PathBuf::from(format!("file-{index}.txt"))))
        .collect();
    picker.filtered = (0..picker.candidates.len()).collect();
    for character in "file-9".chars() {
        editor.update(AppEvent::TextInput(character));
    }
    let cache_before = editor.picker.as_ref().unwrap().ranking_cache.clone();

    editor.update(Command::PickerBackspace.into());

    let picker = editor.picker.as_ref().unwrap();
    assert_eq!(picker.query, "file-");
    assert_eq!(picker.ranking_cache, cache_before);
    assert_eq!(
        picker.filtered,
        cache_before
            .iter()
            .find(|(query, _)| query == "file-")
            .unwrap()
            .1
    );
}

#[test]
fn save_emits_write_effect_and_marks_the_document_saved() {
    let mut editor = Editor::default();
    let path = PathBuf::from("/tmp/save-test.txt");
    editor.open_paths([path.clone()]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("old\n".to_owned()),
    }));
    let before = crate::document::DiskState {
        size: 4,
        modified_nanos: 1,
    };
    editor.update(AppEvent::Io(IoEvent::DiskStateObserved {
        id: DocumentId(1),
        result: Ok(Some(before)),
    }));
    editor.update(AppEvent::TextInput('x'));

    let effects = editor.update(Command::Save.into());
    assert_eq!(
        effects,
        vec![Effect::WriteFile {
            doc: DocumentId(1),
            path: path.clone(),
            contents: "xold\n".to_owned(),
            expected: Some(before),
        }]
    );

    let effects = editor.update(AppEvent::Io(IoEvent::FileSaved {
        id: DocumentId(1),
        result: Ok(()),
    }));
    assert!(matches!(
        effects.as_slice(),
        [Effect::ComputeGitStatus {
            doc: DocumentId(1),
            path: git_path,
        }] if git_path == &path
    ));
    assert!(!editor.active_buffer().unwrap().modified);

    let effects = editor.update(AppEvent::Io(IoEvent::DiskStateObserved {
        id: DocumentId(1),
        result: Ok(Some(crate::document::DiskState {
            size: 5,
            modified_nanos: 2,
        })),
    }));
    assert!(effects.is_empty());

    editor.update(AppEvent::TextInput('y'));
    editor.update(Command::Undo.into());
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "xold\n");
    assert!(!editor.active_buffer().unwrap().modified);

    editor.update(Command::Undo.into());
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "old\n");
    assert!(editor.active_buffer().unwrap().modified);
}

#[test]
fn observing_a_missing_file_is_not_an_error_and_clears_external_change() {
    let mut editor = Editor::default();
    let path = PathBuf::from("/tmp/does-not-exist.txt");
    editor.open_paths([path.clone()]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok(String::new()),
    }));

    // A stale external-change flag should be cleared, and a missing file must
    // not leave an error stuck on the status line.
    editor
        .documents
        .get_mut(&DocumentId(1))
        .unwrap()
        .external_changed = true;
    let effects = editor.update(AppEvent::Io(IoEvent::DiskStateObserved {
        id: DocumentId(1),
        result: Ok(None),
    }));

    assert!(effects.is_empty());
    assert_eq!(editor.status(), None);
    let document = editor.documents.get(&DocumentId(1)).unwrap();
    assert!(!document.external_changed);
    assert_eq!(document.disk_state, None);
}

#[test]
fn an_idle_tick_checks_disk_state_without_forcing_a_redraw() {
    let mut editor = Editor::default();
    editor.take_dirty(); // clear any startup dirty

    let effects = editor.update(AppEvent::Tick);

    // The tick still polls disk state, but with nothing visible changing it must
    // not mark the editor dirty — that would repaint every 2s while idle.
    assert!(
        !editor.take_dirty(),
        "an idle tick should not force a redraw"
    );
    assert!(matches!(effects.as_slice(), [Effect::CheckDiskStates(_)]));
}

#[test]
fn a_tick_with_a_visible_toast_keeps_redrawing() {
    let mut editor = Editor::default();
    editor.notify(ToastLevel::Error, "boom");
    editor.take_dirty(); // clear the dirty the toast itself set

    editor.update(AppEvent::Tick);

    assert!(
        editor.take_dirty(),
        "a visible toast must keep repainting so it can expire on time"
    );
}

#[test]
fn an_identical_diagnostic_resend_does_not_force_a_redraw() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("fn main() {}\n".to_owned()));
    editor.documents.get_mut(&DocumentId(0)).unwrap().path = Some(PathBuf::from("/tmp/diag.rs"));
    let event = || {
        AppEvent::Lsp(LspEvent::Diagnostics {
            uri: "file:///tmp/diag.rs".to_owned(),
            diagnostics: vec![crate::lsp::Diagnostic {
                line: 0,
                character: 0,
                end_line: 0,
                end_character: 2,
                severity: crate::lsp::DiagnosticSeverity::Error,
                message: "boom".to_owned(),
            }],
        })
    };

    editor.update(event());
    assert!(editor.take_dirty(), "a new diagnostic set should repaint");

    // rust-analyzer re-publishes the same diagnostics while idle; the identical
    // resend must not wake a redraw (the "0.3% every few seconds" symptom).
    editor.update(event());
    assert!(
        !editor.take_dirty(),
        "an identical diagnostic resend should not repaint"
    );
}

#[test]
fn an_identical_lsp_progress_message_does_not_force_a_redraw() {
    let mut editor = Editor::default();
    let event = || {
        AppEvent::Lsp(LspEvent::Progress {
            server: 1,
            token: "idx".to_owned(),
            message: Some("Indexing".to_owned()),
        })
    };

    editor.update(event());
    assert!(editor.take_dirty(), "a new progress message should repaint");

    editor.update(event());
    assert!(
        !editor.take_dirty(),
        "an identical progress message should not repaint"
    );
}

#[test]
fn save_refreshes_current_buffer_highlighting_and_semantic_tokens() {
    let mut editor = Editor::default();
    let path = PathBuf::from("/tmp/save-test.rs");
    editor.open_paths([path.clone()]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("fn main() {}\n".to_owned()),
    }));
    editor.test_register_server("rust", 7).ready = true;
    editor.test_open_doc(DocumentId(1), 3);
    editor.pending_lsp.insert(
        99,
        PendingLsp::SemanticTokens {
            doc: DocumentId(1),
            version: 3,
        },
    );
    let document = editor.documents.get_mut(&DocumentId(1)).unwrap();
    let editable = document.editable_mut();
    editable.semantic_spans = vec![crate::lsp::SemanticSpan {
        start: CharIdx(0),
        end: CharIdx(2),
        token_kind: "comment".to_owned(),
        token_modifiers: Vec::new(),
    }];
    editable.syntax = crate::highlight::IncrementalHighlighter::new("rust", "// stale\n");

    let effects = editor.update(Command::Save.into());

    // 保存で構文ハイライトは全体再パースされる。セマンティックスパンは
    // 次の応答が届くまで既存の色を保つ(消すとフラットな色に落ちて見える)。
    assert!(!editor.active_buffer().unwrap().semantic_spans.is_empty());
    assert_eq!(
        editor.active_buffer().unwrap().syntax_spans,
        crate::highlight::highlight("rust", "fn main() {}\n").as_slice()
    );
    assert!(!editor.pending_lsp.contains_key(&99));
    assert!(effects.iter().any(|effect| {
        matches!(
            effect,
            Effect::LspRequest {
                server: 7,
                method,
                ..
            } if method == "textDocument/semanticTokens/full"
        )
    }));
    assert!(effects.iter().any(|effect| {
        matches!(
            effect,
            Effect::WriteFile {
                doc: DocumentId(1),
                path: written_path,
                contents,
                ..
            } if written_path == &path && contents == "fn main() {}\n"
        )
    }));
}

#[test]
fn buffer_search_and_replace_use_overlay_state() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste("one two one".to_owned()));
    editor.update(Command::OpenReplace.into());
    for character in "one".chars() {
        editor.update(AppEvent::TextInput(character));
    }
    assert_eq!(editor.search_view().unwrap().items.len(), 2);
    // Enable replace via the checkbox, type the replacement, run it.
    editor.toggle_replace_field();
    editor.update(AppEvent::TextInput('X'));
    editor.run_replace();

    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "X two X");
}

#[test]
fn directory_search_emits_grep_effect() {
    let mut editor = Editor::default();
    editor.update(Command::OpenSearchInDirectory.into());

    let effects = editor.update(AppEvent::TextInput('x'));

    assert!(
        matches!(effects.as_slice(), [Effect::StartGrep { pattern, .. }] if pattern == "(?i)x")
    );
}

#[test]
fn search_pattern_honors_case_word_and_regex_options() {
    let literal = search_pattern(
        "Foo",
        SearchOptions {
            case_sensitive: true,
            whole_word: true,
            regex: false,
        },
    )
    .unwrap();
    assert!(literal.is_match("Foo"));
    assert!(!literal.is_match("foo"));
    assert!(!literal.is_match("xFooy"));

    let regex = search_pattern(
        "foo.+bar",
        SearchOptions {
            regex: true,
            ..SearchOptions::default()
        },
    )
    .unwrap();
    assert!(regex.is_match("FOO xxx BAR"));
}

#[test]
fn save_conflict_requires_explicit_overwrite_confirmation() {
    let mut editor = Editor::default();
    let path = PathBuf::from("/tmp/conflicted.txt");
    editor.open_paths([path.clone()]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("old".to_owned()),
    }));
    editor.update(AppEvent::TextInput('x'));

    editor.update(AppEvent::Io(IoEvent::SaveConflict {
        id: DocumentId(1),
        path: path.clone(),
    }));
    assert!(editor.confirm_view().is_some());
    let effects = editor.update(Command::PickerConfirm.into());

    assert!(matches!(
        effects.as_slice(),
        [Effect::WriteFile {
            doc: DocumentId(1),
            expected: None,
            ..
        }]
    ));
}

#[test]
fn opening_a_grep_hit_selects_the_match_so_the_jump_is_visible() {
    let mut editor = Editor::default();
    editor.update(Command::OpenSearch.into());
    editor.update(Command::CycleSearchScope.into());
    editor.update(Command::CycleSearchScope.into());
    for character in "needle".chars() {
        editor.update(AppEvent::TextInput(character));
    }
    let token = editor.search().unwrap().grep_token.unwrap();
    editor.update(AppEvent::Grep(GrepEvent::Hits {
        token,
        hits: vec![GrepHit {
            path: PathBuf::from("/tmp/grep_jump.txt"),
            line: 0,
            text: "a needle here".to_owned(),
        }],
    }));

    editor.open_search_hit(0);
    // grep gives no column, so the match is re-located in the line: landing on
    // column 0 would leave a click with nothing to show for it.
    let id = editor
        .documents
        .iter()
        .find_map(|(id, document)| {
            (document.path.as_deref() == Some(std::path::Path::new("/tmp/grep_jump.txt")))
                .then_some(*id)
        })
        .expect("document opened");
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id,
        result: Ok("a needle here\n".to_owned()),
    }));

    let selection = editor.active_buffer().unwrap().view.selections.primary();
    assert!(!selection.is_caret(), "the jump left a bare caret");
    let range = selection.range();
    assert_eq!((range.start, range.end), (2, 8), "\"needle\" not selected");
}

#[test]
fn directory_replace_is_confirmed_before_disk_effect() {
    let mut editor = Editor::default();
    editor.update(Command::OpenReplace.into());
    editor.update(Command::CycleSearchScope.into());
    editor.update(Command::CycleSearchScope.into());
    editor.update(AppEvent::TextInput('o'));
    editor.toggle_replace_field();
    editor.update(AppEvent::TextInput('X'));
    // Each keystroke restarts the grep, so feed hits for the latest token.
    let token = editor.search().unwrap().grep_token.unwrap();
    editor.update(AppEvent::Grep(GrepEvent::Hits {
        token,
        hits: vec![GrepHit {
            path: PathBuf::from("/tmp/a.txt"),
            line: 0,
            text: "one".to_owned(),
        }],
    }));

    // Running replace over a directory asks for confirmation first.
    assert!(editor.run_replace().is_empty());
    assert!(editor.confirm_view().is_some());
    let effects = editor.update(Command::PickerConfirm.into());
    assert!(matches!(
        effects.as_slice(),
        [Effect::ReplaceFiles { paths, replacement, .. }]
            if paths == &[PathBuf::from("/tmp/a.txt")] && replacement == "X"
    ));
}

#[test]
fn formatting_command_sends_lsp_request() {
    let mut editor = Editor::default();
    let path = PathBuf::from("/tmp/format.rs");
    editor.open_paths([path]);
    editor.documents.get_mut(&DocumentId(1)).unwrap().language = Some("rust".to_owned());
    editor.test_register_server("rust", 7).ready = true;
    editor.test_open_doc(DocumentId(1), 1);

    let effects = editor.update(Command::Format.into());

    assert!(matches!(
        effects.as_slice(),
        [Effect::LspRequest { server: 7, method, .. }]
            if method == "textDocument/formatting"
    ));
}

#[test]
fn typing_a_trigger_character_requests_signature_help() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("/tmp/call.rs")]);
    editor.documents.get_mut(&DocumentId(1)).unwrap().language = Some("rust".to_owned());
    let server = editor.test_register_server("rust", 1);
    server.ready = true;
    server.signature_help_triggers = vec!["(".to_owned(), ",".to_owned()];
    editor.test_open_doc(DocumentId(1), 1);

    // A non-trigger character asks for nothing.
    assert!(editor.signature_help_after_typing('x').is_empty());

    let effects = editor.signature_help_after_typing('(');
    assert!(matches!(
        effects.as_slice(),
        [Effect::LspRequest { server: 1, method, .. }]
            if method == "textDocument/signatureHelp"
    ));
}

#[test]
fn signature_help_response_highlights_the_active_parameter_and_closes_on_paren() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("/tmp/call.rs")]);
    editor.documents.get_mut(&DocumentId(1)).unwrap().language = Some("rust".to_owned());
    let server = editor.test_register_server("rust", 1);
    server.ready = true;
    server.signature_help_triggers = vec!["(".to_owned()];
    editor.test_open_doc(DocumentId(1), 1);

    let effects = editor.signature_help_after_typing('(');
    let id = match effects.as_slice() {
        [Effect::LspRequest { id, .. }] => *id,
        other => panic!("expected a signature help request, got {other:?}"),
    };

    // "fn add(a: i32, b: i32)" — the second parameter "b: i32" is bytes 15..21.
    let result = serde_json::json!({
        "signatures": [{
            "label": "fn add(a: i32, b: i32)",
            "parameters": [{"label": [7, 13]}, {"label": [15, 21]}],
        }],
        "activeSignature": 0,
        "activeParameter": 1
    });
    editor.update(AppEvent::Lsp(LspEvent::Response {
        id,
        result: Ok(result),
    }));

    let view = editor.signature_help_view().expect("popup shown");
    assert_eq!(view.label, "fn add(a: i32, b: i32)");
    assert_eq!(view.active_parameter, Some((15, 21)));

    // Closing the call dismisses the popup.
    editor.signature_help_after_typing(')');
    assert!(editor.signature_help_view().is_none());
}

#[test]
fn utf16_parameter_offsets_map_across_multibyte_characters() {
    // "aあb": 'あ' is one UTF-16 unit but three bytes.
    assert_eq!(utf16_offset_to_byte("aあb", 0), Some(0));
    assert_eq!(utf16_offset_to_byte("aあb", 1), Some(1));
    assert_eq!(utf16_offset_to_byte("aあb", 2), Some(4));
    assert_eq!(utf16_offset_to_byte("aあb", 3), Some(5));
    assert_eq!(utf16_offset_to_byte("aあb", 9), None);
}

#[test]
fn file_opened_after_lsp_initialization_gets_did_open_and_semantic_tokens() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("/tmp/first.rs")]);
    let startup = editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("fn first() {}".to_owned()),
    }));
    assert!(matches!(
        startup.last(),
        Some(Effect::SpawnLsp { server: 1, .. })
    ));
    editor.update(AppEvent::Lsp(LspEvent::Initialized {
        server: 1,
        incremental_sync: true,
        hover_provider: true,
        semantic_tokens_legend: Some(crate::lsp::SemanticTokensLegend {
            token_types: vec!["function".to_owned()],
            token_modifiers: Vec::new(),
        }),
        signature_help_triggers: Vec::new(),
    }));

    editor.open_paths([PathBuf::from("/tmp/second.rs")]);
    let effects = editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(2),
        result: Ok("fn second() {}".to_owned()),
    }));

    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::LspSend { server: 1, message }
            if message.contains("textDocument/didOpen") && message.contains("second.rs")
    )));
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::LspRequest { server: 1, method, params, .. }
            if method == "textDocument/semanticTokens/full" && params.contains("second.rs")
    )));
}

#[test]
fn a_server_without_semantic_tokens_opens_without_requesting_them_and_reaches_ready() {
    // pylsp advertises no semanticTokensProvider. The editor must not request
    // tokens it will never get, nor sit forever on the "coloring" status.
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("/tmp/script.py")]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("x = 1\n".to_owned()),
    }));
    editor.update(AppEvent::Lsp(LspEvent::Spawned {
        server: 1,
        language: "python".to_owned(),
    }));
    let effects = editor.update(AppEvent::Lsp(LspEvent::Initialized {
        server: 1,
        incremental_sync: true,
        hover_provider: true,
        semantic_tokens_legend: None,
        signature_help_triggers: Vec::new(),
    }));

    // didOpen is still sent, but no semantic-tokens request.
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::LspSend { server: 1, message } if message.contains("textDocument/didOpen")
    )));
    assert!(!effects.iter().any(|effect| matches!(
        effect,
        Effect::LspRequest { method, .. } if method == "textDocument/semanticTokens/full"
    )));

    // The status must not be stuck on "coloring"; once hover answers it is ready.
    assert_ne!(
        editor.active_buffer().unwrap().language_status,
        "<lsp> python: coloring"
    );
    let hover_probe = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::LspRequest { id, method, .. } if method == "textDocument/hover" => Some(*id),
            _ => None,
        })
        .unwrap();
    editor.update(AppEvent::Lsp(LspEvent::Response {
        id: hover_probe,
        result: Ok(serde_json::json!({
            "contents": {"kind": "markdown", "value": "int"}
        })),
    }));
    assert_eq!(
        editor.active_buffer().unwrap().language_status,
        "<lsp> python: ready"
    );
}

#[test]
fn reopening_an_open_file_reuses_the_existing_document() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("/tmp/dup.rs")]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("fn main() {}\n".to_owned()),
    }));
    editor.update(AppEvent::TextInput('x'));
    assert!(editor.active_buffer().unwrap().modified);

    let effects = editor.open_paths([PathBuf::from("/tmp/dup.rs")]);

    // 複製バッファを作らず、未保存編集をディスク内容で潰さない。
    assert_eq!(editor.documents.len(), 1);
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::ReadFile { .. }))
    );
    assert_eq!(
        editor
            .layout
            .active_editor(editor.focus)
            .map(|pane| pane.view.doc),
        Some(DocumentId(1))
    );

    // 保存済みならディスクから同じ文書IDへ再読込する。
    editor
        .documents
        .get_mut(&DocumentId(1))
        .unwrap()
        .editable_mut()
        .mark_saved();
    let effects = editor.open_paths([PathBuf::from("/tmp/dup.rs")]);
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::ReadFile {
            id: DocumentId(1),
            ..
        }
    )));
    assert_eq!(editor.documents.len(), 1);
}

#[test]
fn external_reload_resyncs_full_text_to_lsp_server() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("/tmp/reload.rs")]);
    editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("fn before() {}\n".to_owned()),
    }));
    editor.update(AppEvent::Lsp(LspEvent::Initialized {
        server: 1,
        incremental_sync: true,
        hover_provider: true,
        semantic_tokens_legend: None,
        signature_help_triggers: Vec::new(),
    }));
    assert!(editor.doc_is_opened(DocumentId(1)));
    let version_before = editor.doc_version(DocumentId(1)).unwrap();

    // 外部ツールがディスク上のファイルを書き換えた後の自動再読込。
    let effects = editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("// external\nfn after() {}\n".to_owned()),
    }));

    let did_change = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::LspSend { server: 1, message }
                if message.contains("textDocument/didChange") =>
            {
                Some(message.clone())
            }
            _ => None,
        })
        .expect("再読込後は全文didChangeでサーバーと同期し直す");
    assert!(did_change.contains("fn after() {}"));
    assert_eq!(
        editor.doc_version(DocumentId(1)).unwrap(),
        version_before + 1
    );
}

#[test]
fn loading_config_does_not_start_a_language_server_until_its_file_is_opened() {
    let mut editor = Editor::default();
    editor.set_workspace_root(PathBuf::from("/workspace"));

    // No language file is open, so loading the config must not spawn any
    // server — opening, say, a Markdown file should never wake rust-analyzer.
    let effects = editor.update(AppEvent::ConfigLoaded(Ok(Config::default())));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::SpawnLsp { .. }))
    );

    // Opening a Rust file starts rust-analyzer lazily.
    editor.open_paths([PathBuf::from("main.rs")]);
    let effects = editor.update(AppEvent::Io(IoEvent::FileLoaded {
        id: DocumentId(1),
        result: Ok("fn main() {}".to_owned()),
    }));
    assert!(matches!(
        effects
            .iter()
            .find(|effect| matches!(effect, Effect::SpawnLsp { .. })),
        Some(Effect::SpawnLsp {
            language,
            root,
            ..
        }) if language == "rust" && root == &PathBuf::from("/workspace")
    ));
}

#[test]
fn status_reports_language_and_lsp_lifecycle() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("main.rs")]);
    editor
        .documents
        .get_mut(&DocumentId(1))
        .unwrap()
        .load_text("fn main() { let alpha = beta; gamma(delta); epsilon(); }");
    editor.update(AppEvent::ConfigLoaded(Ok(Config::default())));
    assert_eq!(
        editor.active_buffer().unwrap().language_status,
        "<lsp> rust: starting"
    );

    editor.update(AppEvent::Lsp(LspEvent::Spawned {
        server: 1,
        language: "rust".to_owned(),
    }));
    assert_eq!(
        editor.active_buffer().unwrap().language_status,
        "<lsp> rust: initializing"
    );

    let effects = editor.update(AppEvent::Lsp(LspEvent::Initialized {
        server: 1,
        incremental_sync: true,
        hover_provider: true,
        semantic_tokens_legend: Some(crate::lsp::SemanticTokensLegend {
            token_types: vec!["function".to_owned()],
            token_modifiers: Vec::new(),
        }),
        signature_help_triggers: Vec::new(),
    }));
    assert_eq!(
        editor.active_buffer().unwrap().language_status,
        "<lsp> rust: coloring"
    );

    let semantic_request = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::LspRequest { id, method, .. }
                if method == "textDocument/semanticTokens/full" =>
            {
                Some(*id)
            }
            _ => None,
        })
        .unwrap();
    let hover_probe = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::LspRequest { id, method, .. } if method == "textDocument/hover" => Some(*id),
            _ => None,
        })
        .unwrap();
    editor.update(AppEvent::Lsp(LspEvent::Response {
        id: semantic_request,
        result: Ok(serde_json::json!({"data": [0, 0, 1, 0, 0]})),
    }));
    assert_eq!(
        editor.active_buffer().unwrap().language_status,
        "<lsp> rust: checking hover"
    );
    // hover が一度返った時点で ready(候補を巡回し続けて待たせない)。
    editor.update(AppEvent::Lsp(LspEvent::Response {
        id: hover_probe,
        result: Ok(serde_json::json!({
            "contents": {"kind": "markdown", "value": "fn main()"}
        })),
    }));
    assert_eq!(
        editor.active_buffer().unwrap().language_status,
        "<lsp> rust: ready"
    );

    editor.update(AppEvent::Lsp(LspEvent::Progress {
        server: 1,
        token: "index".to_owned(),
        message: Some("Indexing 20%".to_owned()),
    }));
    editor.update(AppEvent::Lsp(LspEvent::Progress {
        server: 1,
        token: "check".to_owned(),
        message: Some("Checking".to_owned()),
    }));
    editor.update(AppEvent::Lsp(LspEvent::Progress {
        server: 1,
        token: "index".to_owned(),
        message: None,
    }));
    assert_eq!(
        editor.active_buffer().unwrap().language_status,
        "<lsp> rust: updating (Checking)"
    );
    editor.update(AppEvent::Lsp(LspEvent::Progress {
        server: 1,
        token: "check".to_owned(),
        message: None,
    }));

    editor.update(AppEvent::TextInput('x'));
    assert_eq!(
        editor.active_buffer().unwrap().language_status,
        "<lsp> rust: updating"
    );

    editor.update(AppEvent::Lsp(LspEvent::Exited {
        server: 1,
        error: Some("not found".to_owned()),
    }));
    assert_eq!(
        editor.active_buffer().unwrap().language_status,
        "<lsp> rust: not found"
    );
}

#[test]
fn crashed_lsp_restarts_three_times_with_exponential_backoff() {
    let mut editor = Editor::default();
    editor.test_register_server("rust", 1);
    for expected_delay in [500, 1_000, 2_000] {
        assert_eq!(
            editor.update(AppEvent::Lsp(LspEvent::Exited {
                server: 1,
                error: Some("crashed".to_owned()),
            })),
            vec![Effect::ScheduleLspRestart {
                server: 1,
                delay_ms: expected_delay,
            }]
        );
    }

    assert!(
        editor
            .update(AppEvent::Lsp(LspEvent::Exited {
                server: 1,
                error: Some("crashed".to_owned()),
            }))
            .is_empty()
    );
    assert_eq!(
        editor.server(1).and_then(|server| server.error.as_deref()),
        Some("crashed")
    );
}

#[test]
fn failed_lsp_initialization_never_reports_ready() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("main.rs")]);
    editor.update(AppEvent::ConfigLoaded(Ok(Config::default())));
    editor.update(AppEvent::Lsp(LspEvent::Spawned {
        server: 1,
        language: "rust".to_owned(),
    }));

    editor.update(AppEvent::Lsp(LspEvent::InitializationFailed {
        server: 1,
        error: Some("initialize rejected".to_owned()),
    }));

    assert_eq!(
        editor.active_buffer().unwrap().language_status,
        "<lsp> rust: error"
    );
    assert!(!editor.server_ready(1));
}

#[test]
fn markdown_status_does_not_report_the_workspace_rust_lsp() {
    let mut editor = Editor::default();
    editor.open_paths([PathBuf::from("README.md")]);
    editor.update(AppEvent::ConfigLoaded(Ok(Config::default())));

    assert_eq!(
        editor.active_buffer().unwrap().language_status,
        "<syntax> markdown"
    );
}

#[test]
fn completion_excludes_the_candidate_identical_to_the_typed_prefix() {
    let mut editor = Editor::default();
    let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
    document.path = Some(PathBuf::from("main.rs"));
    document.language = Some("rust".to_owned());
    let server = editor.test_register_server("rust", 1);
    server.spawned = true;
    server.ready = true;
    editor.test_open_doc(DocumentId(0), 1);

    editor.update(AppEvent::TextPaste("let collections".to_owned()));
    let effects = editor.request_completion(true);
    let request = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::LspRequest { id, .. } => Some(*id),
            _ => None,
        })
        .unwrap();
    editor.update(AppEvent::Lsp(LspEvent::Response {
        id: request,
        result: Ok(serde_json::json!([
            {"label": "collections"},
            {"label": "collections_mut"}
        ])),
    }));

    let completion = editor.completion_view().unwrap();
    assert_eq!(completion.items, vec!["collections_mut"]);
    assert_eq!(completion.anchor, CharIdx(4));
}

#[test]
fn method_matching_the_prefix_stays_because_it_completes_to_a_call() {
    let mut editor = Editor::default();
    let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
    document.path = Some(PathBuf::from("main.rs"));
    document.language = Some("rust".to_owned());
    let server = editor.test_register_server("rust", 1);
    server.spawned = true;
    server.ready = true;
    editor.test_open_doc(DocumentId(0), 1);

    editor.update(AppEvent::TextPaste("s.push".to_owned()));
    let request = editor
        .request_completion(true)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::LspRequest { id, .. } => Some(id),
            _ => None,
        })
        .unwrap();
    editor.update(AppEvent::Lsp(LspEvent::Response {
        id: request,
        // Both are methods; `push` equals the prefix but completes to `push()`.
        result: Ok(serde_json::json!([
            {"label": "push", "kind": 2},
            {"label": "push_str", "kind": 2}
        ])),
    }));

    let completion = editor.completion_view().unwrap();
    assert!(
        completion.items.contains(&"push".to_owned()),
        "push was dropped: {:?}",
        completion.items
    );
    assert!(completion.items.contains(&"push_str".to_owned()));
}

#[test]
fn starting_rename_dismisses_the_hover_and_completion_popups() {
    let mut editor = Editor::default();
    let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
    document.path = Some(PathBuf::from("main.rs"));
    document.language = Some("rust".to_owned());
    editor.test_register_server("rust", 1).ready = true;
    editor.test_open_doc(DocumentId(0), 1);
    editor.update(AppEvent::TextPaste("value".to_owned()));
    editor.hover = Some("診断: unused variable".to_owned());

    editor.update(Command::Rename.into());

    assert!(editor.rename_view().is_some(), "rename prompt should open");
    assert!(
        editor.hover_view().is_none(),
        "the hover/diagnostic popup should be cleared so it can't overlap rename"
    );
    assert!(editor.completion_view().is_none());
}

#[test]
fn function_completion_inserts_parentheses_and_places_caret_inside() {
    let mut editor = Editor::default();
    let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
    document.path = Some(PathBuf::from("main.rs"));
    document.language = Some("rust".to_owned());
    editor.test_register_server("rust", 1).ready = true;
    editor.test_open_doc(DocumentId(0), 1);
    editor.update(AppEvent::TextPaste("cur".to_owned()));

    let request = editor
        .request_completion(true)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::LspRequest { id, .. } => Some(id),
            _ => None,
        })
        .unwrap();
    editor.update(AppEvent::Lsp(LspEvent::Response {
        id: request,
        result: Ok(serde_json::json!([{
            "label": "current_dir",
            "insertText": "current_dir",
            "kind": 3
        }])),
    }));
    editor.update(Command::PickerConfirm.into());

    let buffer = editor.active_buffer().unwrap();
    assert_eq!(buffer.text.to_string(), "current_dir()");
    assert_eq!(
        buffer.text.char(buffer.view.selections.primary().head.0),
        ')'
    );
}

#[test]
fn function_completion_in_an_import_statement_does_not_add_parentheses() {
    let mut editor = Editor::default();
    let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
    document.path = Some(PathBuf::from("main.rs"));
    document.language = Some("rust".to_owned());
    editor.test_register_server("rust", 1).ready = true;
    editor.test_open_doc(DocumentId(0), 1);
    editor.update(AppEvent::TextPaste("use crate::cur".to_owned()));

    let request = editor
        .request_completion(true)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::LspRequest { id, .. } => Some(id),
            _ => None,
        })
        .unwrap();
    editor.update(AppEvent::Lsp(LspEvent::Response {
        id: request,
        result: Ok(serde_json::json!([{
            "label": "current_dir",
            "insertText": "current_dir",
            "kind": 3
        }])),
    }));
    editor.update(Command::PickerConfirm.into());

    assert_eq!(
        editor.active_buffer().unwrap().text.to_string(),
        "use crate::current_dir"
    );
}

/// Drive one completion round-trip and return the resulting buffer text.
fn complete_once(language: &str, path: &str, typed: &str, item: serde_json::Value) -> String {
    let mut editor = Editor::default();
    let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
    document.path = Some(PathBuf::from(path));
    document.language = Some(language.to_owned());
    editor.test_register_server(language, 1).ready = true;
    editor.test_open_doc(DocumentId(0), 1);
    editor.update(AppEvent::TextPaste(typed.to_owned()));

    let request = editor
        .request_completion(true)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::LspRequest { id, .. } => Some(id),
            _ => None,
        })
        .unwrap();
    editor.update(AppEvent::Lsp(LspEvent::Response {
        id: request,
        result: Ok(serde_json::json!([item])),
    }));
    editor.update(Command::PickerConfirm.into());
    editor.active_buffer().unwrap().text.to_string()
}

#[test]
fn a_python_class_completion_is_called_and_gets_parentheses() {
    // pylsp reports builtins like `enumerate` as CLASS (kind 7), not FUNCTION —
    // in Python the class name *is* the call, so it still takes parentheses.
    assert_eq!(
        complete_once(
            "python",
            "main.py",
            "enum",
            serde_json::json!({"label": "enumerate", "insertText": "enumerate", "kind": 7}),
        ),
        "enumerate()"
    );
}

#[test]
fn a_rust_struct_completion_is_not_called_and_keeps_no_parentheses() {
    // A Rust type is not constructed by calling its name (`Vec::new()`), so the
    // same CLASS kind must not gain parentheses here.
    assert_eq!(
        complete_once(
            "rust",
            "main.rs",
            "Ve",
            serde_json::json!({"label": "Vec", "insertText": "Vec", "kind": 7}),
        ),
        "Vec"
    );
}

#[test]
fn a_constructor_completion_gets_parentheses() {
    assert_eq!(
        complete_once(
            "python",
            "main.py",
            "Poi",
            serde_json::json!({"label": "Point", "insertText": "Point", "kind": 4}),
        ),
        "Point()"
    );
}

#[test]
fn malformed_automatic_completion_response_does_not_replace_the_status() {
    let mut editor = Editor::default();
    let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
    document.path = Some(PathBuf::from("main.rs"));
    document.language = Some("rust".to_owned());
    editor.test_register_server("rust", 1).ready = true;
    editor.test_open_doc(DocumentId(0), 1);
    editor.update(AppEvent::TextPaste("cur".to_owned()));
    editor.status = Some("保存しました".to_owned());
    let request = editor
        .request_completion(false)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::LspRequest { id, .. } => Some(id),
            _ => None,
        })
        .unwrap();

    editor.update(AppEvent::Lsp(LspEvent::Response {
        id: request,
        result: Err("server cancelled request".to_owned()),
    }));

    assert_eq!(editor.status(), Some("保存しました"));
}

#[test]
fn indent_and_comment_apply_to_every_selected_line() {
    let mut editor = Editor::default();
    editor.documents.get_mut(&DocumentId(0)).unwrap().language = Some("rust".to_owned());
    editor.update(AppEvent::TextPaste("one\ntwo".to_owned()));
    editor.update(Command::SelectAll.into());

    editor.update(Command::Indent.into());
    assert_eq!(
        editor.active_buffer().unwrap().text.to_string(),
        "    one\n    two"
    );

    editor.update(Command::SelectAll.into());
    editor.update(Command::ToggleComment.into());
    assert_eq!(
        editor.active_buffer().unwrap().text.to_string(),
        "    // one\n    // two"
    );
    editor.update(Command::SelectAll.into());
    editor.update(Command::ToggleComment.into());
    assert_eq!(
        editor.active_buffer().unwrap().text.to_string(),
        "    one\n    two"
    );
}

#[test]
fn tab_without_a_selection_inserts_configured_spaces_at_the_caret() {
    let mut editor = Editor::default();
    editor.config.editor.tab_size = 3;
    editor.update(AppEvent::TextPaste("ab".to_owned()));
    editor.update(
        Command::Move {
            direction: Direction::Left,
            unit: Unit::Character,
            extend: false,
        }
        .into(),
    );

    editor.update(Command::Indent.into());

    let buffer = editor.active_buffer().unwrap();
    assert_eq!(buffer.text.to_string(), "a   b");
    assert_eq!(buffer.view.selections.primary().head, CharIdx(4));

    editor.update(Command::DeleteBackward.into());
    let buffer = editor.active_buffer().unwrap();
    assert_eq!(buffer.text.to_string(), "a  b");
    assert_eq!(buffer.view.selections.primary().head, CharIdx(3));
}

#[test]
fn bracketed_multiline_paste_is_inserted_literally_and_normalizes_line_endings() {
    let mut editor = Editor::default();

    editor.update(AppEvent::TextPaste("if ready {\r\n  value\r\n}".to_owned()));

    assert_eq!(
        editor.active_buffer().unwrap().text.to_string(),
        "if ready {\n  value\n}"
    );
}

#[test]
fn typed_opening_delimiters_insert_pairs_without_treating_paste_as_typing() {
    let mut editor = Editor::default();

    editor.update(AppEvent::TextInput('['));
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "[]");
    assert_eq!(
        editor
            .active_buffer()
            .unwrap()
            .view
            .selections
            .primary()
            .head,
        CharIdx(1)
    );
    editor.update(Command::DeleteBackward.into());
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "");

    editor.update(AppEvent::TextInput('{'));
    editor.update(AppEvent::TextInput('}'));
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "{}");
    assert_eq!(
        editor
            .active_buffer()
            .unwrap()
            .view
            .selections
            .primary()
            .head,
        CharIdx(2)
    );
    editor.update(Command::DeleteBackward.into());
    editor.update(Command::DeleteBackward.into());
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "");

    editor.update(AppEvent::TextPaste("[literal]".to_owned()));
    assert_eq!(
        editor.active_buffer().unwrap().text.to_string(),
        "[literal]"
    );
}

#[test]
fn ctrl_c_keeps_the_find_pane_open() {
    let mut editor = Editor::default();
    editor.update(Command::OpenSearch.into());

    editor.update(Command::Cancel.into());

    assert!(editor.search_view().is_some());
    assert_eq!(editor.focus(), Focus::Overlay);
}

#[test]
fn mouse_wheel_scrolls_the_terminal_pane_scrollback() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 6 });
    let token = open_shell(&mut editor);
    editor.update(AppEvent::Terminal(TerminalEvent::Output {
        token,
        bytes: (0..20)
            .map(|line| format!("line {line}\r\n"))
            .collect::<String>()
            .into_bytes(),
    }));
    assert_eq!(
        editor.shell.as_ref().unwrap().parser.screen().scrollback(),
        0
    );

    editor.update(AppEvent::Mouse(MouseInput {
        event: MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 30,
            row: 2,
            modifiers: KeyModifiers::NONE,
        },
        clicks: 0,
    }));

    assert_eq!(
        editor.shell.as_ref().unwrap().parser.screen().scrollback(),
        3
    );
}

#[test]
fn shell_paste_preserves_child_bracketed_paste_mode() {
    let mut editor = Editor::default();
    editor.update(AppEvent::Resize { cols: 40, rows: 6 });
    let token = open_shell(&mut editor);
    editor.update(AppEvent::Terminal(TerminalEvent::Output {
        token,
        bytes: b"\x1b[?2004h".to_vec(),
    }));

    let effects = editor.update(AppEvent::TextPaste("one\ntwo".to_owned()));

    assert_eq!(
        effects,
        vec![Effect::TerminalInput(
            b"\x1b[200~one\ntwo\x1b[201~".to_vec()
        )]
    );
}

#[test]
fn tab_then_newline_has_exactly_one_newline_and_no_tab_in_space_mode() {
    let mut editor = Editor::default();
    editor.documents.get_mut(&DocumentId(0)).unwrap().language = Some("rust".to_owned());

    editor.update(Command::Indent.into());
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "    ");
    assert_eq!(
        editor
            .active_buffer()
            .unwrap()
            .view
            .selections
            .primary()
            .head,
        CharIdx(4)
    );

    editor.update(Command::InsertNewline.into());

    let buffer = editor.active_buffer().unwrap();
    assert_eq!(buffer.text.to_string(), "    \n    ");
    assert_eq!(
        buffer
            .text
            .chars()
            .filter(|character| *character == '\n')
            .count(),
        1
    );
    assert!(!buffer.text.to_string().contains('\t'));
    assert_eq!(buffer.view.selections.primary().head, CharIdx(9));
}

#[test]
fn selected_lines_indent_together_and_outdent_clamps_to_existing_space() {
    let mut editor = Editor::default();
    editor.update(AppEvent::TextPaste(" a\n    b".to_owned()));
    editor.update(Command::SelectAll.into());

    editor.update(Command::Outdent.into());
    assert_eq!(editor.active_buffer().unwrap().text.to_string(), "a\nb");

    editor.update(Command::Undo.into());
    assert_eq!(
        editor.active_buffer().unwrap().text.to_string(),
        " a\n    b"
    );
    assert_eq!(
        editor
            .active_buffer()
            .unwrap()
            .view
            .selections
            .primary()
            .range(),
        0..8
    );
}

#[test]
fn makefile_tab_inserts_a_real_tab() {
    let mut editor = Editor::default();
    editor.documents.get_mut(&DocumentId(0)).unwrap().language = Some("make".to_owned());

    editor.update(Command::Indent.into());

    let buffer = editor.active_buffer().unwrap();
    assert_eq!(buffer.text.to_string(), "\t");
    assert_eq!(buffer.view.selections.primary().head, CharIdx(1));
    assert_eq!(buffer.tab_size, 4);
}
