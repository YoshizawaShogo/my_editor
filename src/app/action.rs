#[derive(Clone, Copy)]
pub enum ReplayableAction {
    GitHunk { forward: bool },
    Find(FindKind, char),
    Diagnostic { error_only: bool, forward: bool },
    Search { forward: bool },
}

#[derive(Clone, Copy)]
pub enum PendingNormalAction {
    JumpPrefix,
}

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub enum PendingOperator {
    Change,
    Delete,
    Yank,
}

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub enum FindKind {
    Forward,
    Backward,
    TillForward,
    TillBackward,
}
