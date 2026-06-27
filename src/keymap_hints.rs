pub struct KeyHint {
    pub key: &'static str,
    pub description: &'static str,
}

pub static TOP_LEVEL_HINTS: &[KeyHint] = &[
    KeyHint {
        key: "g",
        description: "Jump / LSP ナビ",
    },
    KeyHint {
        key: "e",
        description: "診断",
    },
    KeyHint {
        key: "c",
        description: "Change（変更）",
    },
    KeyHint {
        key: "d",
        description: "Delete（削除）",
    },
    KeyHint {
        key: "y",
        description: "Yank（コピー）",
    },
];

pub static GO_PREFIX_HINTS: &[KeyHint] = &[
    KeyHint {
        key: "d",
        description: "定義へジャンプ",
    },
    KeyHint {
        key: "i",
        description: "実装へジャンプ",
    },
    KeyHint {
        key: "D",
        description: "宣言へジャンプ",
    },
    KeyHint {
        key: "r",
        description: "参照一覧",
    },
    KeyHint {
        key: "t",
        description: "ファイル先頭",
    },
    KeyHint {
        key: "T",
        description: "ファイル末尾",
    },
    KeyHint {
        key: "g",
        description: "次の Git hunk",
    },
    KeyHint {
        key: "G",
        description: "前の Git hunk",
    },
    KeyHint {
        key: "w",
        description: "次の診断",
    },
    KeyHint {
        key: "W",
        description: "前の診断",
    },
    KeyHint {
        key: "e",
        description: "次のエラー",
    },
    KeyHint {
        key: "E",
        description: "前のエラー",
    },
    KeyHint {
        key: "f",
        description: "検索繰り返し（前方）",
    },
    KeyHint {
        key: "F",
        description: "検索繰り返し（後方）",
    },
    KeyHint {
        key: "Esc",
        description: "キャンセル",
    },
];

pub static DIAGNOSTIC_PREFIX_HINTS: &[KeyHint] = &[
    KeyHint {
        key: "d",
        description: "診断ポップアップ",
    },
    KeyHint {
        key: "w",
        description: "診断一覧（全種類）",
    },
    KeyHint {
        key: "e",
        description: "診断一覧（エラーのみ）",
    },
    KeyHint {
        key: "W",
        description: "ワークスペース診断（全種類）",
    },
    KeyHint {
        key: "E",
        description: "ワークスペース診断（エラーのみ）",
    },
    KeyHint {
        key: "Esc",
        description: "キャンセル",
    },
];

pub static CHANGE_OPERATOR_HINTS: &[KeyHint] = &[
    KeyHint {
        key: "c",
        description: "行全体を変更",
    },
    KeyHint {
        key: "f/F/t/T",
        description: "文字まで変更",
    },
    KeyHint {
        key: "i",
        description: "構文単位で変更",
    },
    KeyHint {
        key: "Esc",
        description: "キャンセル",
    },
];

pub static DELETE_OPERATOR_HINTS: &[KeyHint] = &[
    KeyHint {
        key: "d",
        description: "行全体を削除",
    },
    KeyHint {
        key: "f/F/t/T",
        description: "文字まで削除",
    },
    KeyHint {
        key: "i",
        description: "構文単位で削除",
    },
    KeyHint {
        key: "Esc",
        description: "キャンセル",
    },
];

pub static YANK_OPERATOR_HINTS: &[KeyHint] = &[
    KeyHint {
        key: "y",
        description: "行全体をヤンク",
    },
    KeyHint {
        key: "f/F/t/T",
        description: "文字までヤンク",
    },
    KeyHint {
        key: "i",
        description: "構文単位でヤンク",
    },
    KeyHint {
        key: "Esc",
        description: "キャンセル",
    },
];
