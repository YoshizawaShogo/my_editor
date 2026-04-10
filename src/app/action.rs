#[derive(Clone, Copy)]
pub enum ReplayableAction {
    GitHunk { forward: bool },
    Find { kind: FindKind, ch: char },
    Diagnostic { error_only: bool, forward: bool },
    Search { forward: bool },
}

#[derive(Clone, Copy)]
pub enum PendingNormalAction {
    GoPrefix,
    DiagnosticPrefix,
    Find { kind: FindKind },
    Operator { operator: PendingOperator },
    OperatorFind { operator: PendingOperator, find_kind: FindKind },
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
