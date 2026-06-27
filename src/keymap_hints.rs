pub struct KeyHint {
    pub key: &'static str,
    pub description: &'static str,
}

pub static TOP_LEVEL_HINTS: &[KeyHint] = &[
    KeyHint {
        key: "c-f",
        description: "検索",
    },
    KeyHint {
        key: "c-h",
        description: "置換",
    },
    KeyHint {
        key: "c-t",
        description: "ファイルを開く",
    },
    KeyHint {
        key: "c-s",
        description: "保存",
    },
    KeyHint {
        key: "c-w",
        description: "バッファを閉じる",
    },
    KeyHint {
        key: "c-z/y",
        description: "Undo / Redo",
    },
    KeyHint {
        key: "c-c",
        description: "コピー（選択/行）",
    },
    KeyHint {
        key: "c-x",
        description: "切り取り（選択/行）",
    },
    KeyHint {
        key: "c-v",
        description: "貼り付け",
    },
    KeyHint {
        key: "c-a",
        description: "全選択",
    },
    KeyHint {
        key: "c-d",
        description: "単語/次の同じ単語を選択",
    },
    KeyHint {
        key: "c-q",
        description: "コメントトグル",
    },
    KeyHint {
        key: "c-j",
        description: "Jump / LSP ナビ",
    },
    KeyHint {
        key: "c-n/p",
        description: "リプレイ（次/前）",
    },
    KeyHint {
        key: "c-↑/↓",
        description: "半ページ移動",
    },
    KeyHint {
        key: "c-g",
        description: "行番号ジャンプ",
    },
    KeyHint {
        key: "F2",
        description: "リネーム",
    },
    KeyHint {
        key: "F4",
        description: "終了",
    },
    KeyHint {
        key: "F8",
        description: "折り返しトグル",
    },
    KeyHint {
        key: "Alt+h",
        description: "このヒントを隠す",
    },
];

pub static JUMP_PREFIX_HINTS: &[KeyHint] = &[
    KeyHint {
        key: "d",
        description: "定義へジャンプ",
    },
    KeyHint {
        key: "i",
        description: "実装へジャンプ",
    },
    KeyHint {
        key: "r",
        description: "参照一覧",
    },
    KeyHint {
        key: "D",
        description: "宣言へジャンプ",
    },
    KeyHint {
        key: "e",
        description: "診断ポップアップ",
    },
    KeyHint {
        key: "w/W",
        description: "次/前の診断",
    },
    KeyHint {
        key: "n/N",
        description: "次/前のエラー",
    },
    KeyHint {
        key: "g/G",
        description: "次/前の Git hunk",
    },
    KeyHint {
        key: "t",
        description: "ファイル先頭",
    },
    KeyHint {
        key: "b",
        description: "ファイル末尾",
    },
    KeyHint {
        key: "f/F",
        description: "検索繰り返し（前/後）",
    },
    KeyHint {
        key: "Esc",
        description: "キャンセル",
    },
];
