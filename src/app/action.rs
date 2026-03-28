#[derive(Clone, Copy)]
pub enum ReplayableAction {
    GitHunk { forward: bool },
    Find(FindKind, char),
    Diagnostic { error_only: bool, forward: bool },
    Search { forward: bool },
}

#[derive(Clone, Copy)]
pub enum PendingNormalAction {
    GoPrefix,
    DiagnosticPrefix,
    Find(FindKind),
    Operator(PendingOperator),
    OperatorFind(PendingOperator, FindKind),
}

#[derive(Clone, Copy)]
pub enum PendingOperator {
    Change,
    Delete,
    Yank,
}

#[derive(Clone, Copy)]
pub enum FindKind {
    Forward,
    Backward,
    TillForward,
    TillBackward,
}
