# my_editor 設計ドキュメント

## プロジェクト概要

Rust 製の TUI テキストエディタ。ratatui + crossterm で描画し、ropey でテキストを管理する。
Rust ファイルの編集に特化し、rust-analyzer LSP 統合によるリッチな言語機能を提供する。

操作体系は Vim 風のモードベースだが、キーバインドは独自配置。

---

## 設計原則

- **操作者の認知コスト削減を重要視する**: キーバインドは覚えやすい根拠を持たせ、覚えていなくても UI が誘導する
- **シンプルさより機能性**: 検索・LSP など核心機能は VSCode 相当の水準を目指す
- **Rust 専用を起点に汎用化**: 最初は rust-analyzer 専用で設計し、将来的に他言語の LSP サーバーにも対応できる構造にする

---

## 設計目標

- Vim の操作感（Normal / Insert / Shell モード）をベースとした独自キーバインド
- rust-analyzer LSP 統合（補完・診断・リネーム・定義ジャンプ等）、将来的に多言語対応
- シェル統合ターミナルペイン（PTY + vt100）
- ropey (Rope データ構造) による効率的なテキスト編集
- 50ms ポーリングによるリアルタイム描画

---

## アーキテクチャ

```
App (main state machine)
├── Workspace (複数 Document 管理)
│   ├── EditableDocument (ropey::Rope + undo/redo スタック)
│   ├── LargeFileDocument (10MB 超 → 64KB ウィンドウ読み込み、読み取り専用)
│   └── ScratchDocument (検索結果・診断リスト表示用バッファ)
├── LspClientState (rust-analyzer、tokio 非同期、バックグラウンドスレッド)
├── ShellState (PTY + vt100 パーサ、/bin/sh or $SHELL)
├── PickerState (ファジーファイルピッカー、fuzzy-matcher)
└── render (ratatui フレーム描画)
```

### App の主要フィールド

| フィールド | 役割 |
|---|---|
| `mode: Mode` | 現在のモード（Normal / Insert / Shell） |
| `workspace: Workspace` | 開いているドキュメント一覧と現在インデックス |
| `cursor: CursorState` | カーソル位置（display_row, display_col） |
| `viewport_row` | スクロール位置 |
| `pending_normal_action` | マルチキーシーケンス待機状態 |
| `lsp: LspClientState` | LSP クライアント状態 |
| `shell: ShellState` | ターミナルペイン状態 |
| `picker: PickerState` | ファイルピッカー状態 |
| `layout_mode: LayoutMode` | Single / Dual / TerminalSplit |
| `yank_buffer: YankBuffer` | charwise / linewise クリップボード |
| `jump_history` | ジャンプ前後の位置履歴 |

---

## モード体系

| モード | 説明 |
|---|---|
| `Normal` | Vim 風コマンド入力。状態機械（`PendingNormalAction`）でマルチキーシーケンスを処理 |
| `Insert` | テキスト入力。LSP 補完を自動表示 |
| `Shell` | ターミナルペインにキー入力をそのまま転送 |

### NormalAction の状態機械

```
KeyEvent (crossterm)
  → NormalInput (正規化)
  → transition_normal_input() で PendingNormalAction を更新
  → NormalDecision { Ignore / Quit / SetPending / Action }
  → apply_normal_action() でアプリ状態を変更
```

`PendingNormalAction` の種類：
- `GoPrefix` — `g` プレフィックス待機
- `DiagnosticPrefix` — `e` プレフィックス待機
- `Find { kind }` — `f/F/t/T` の文字入力待機
- `Operator { operator }` — `c/d/y` のモーション待機
- `OperatorFind { operator, find_kind }` — オペレータ + 検索の組み合わせ

---

## キーバインド（Normal モード主要キー）

Vim とは異なる独自配置。

### 配置の考え方

`i/j/k/l` を **IJKL 十字キー配置**（WASD 風）とした：

```
    i (上)
j (左)  l (右)
    k (下)
```

これにより `h` が空き、インサートモードのキーとして転用できた。Vim の `h`（左移動）を `j` に移した形。

### 移動

| キー | 動作 |
|---|---|
| `i` | 上 |
| `k` | 下 |
| `j` | 左 |
| `l` | 右 |
| `Home` / `End` | 行頭 / 行末 |
| `Ctrl+d` / `Ctrl+u` | 半画面下 / 上 |

### モード切り替え

| キー | 動作 |
|---|---|
| `h` | Insert モード（カーソル位置から） |
| `a` | Insert モード（カーソルの次から） |
| `o` | 下に新規行を作成して Insert |
| `Ctrl+Space` | Shell モード（ターミナルペイン） |

### 編集

| キー | 動作 |
|---|---|
| `u` / `U` | undo / redo |
| `p` / `P` | ペースト（後 / 前） |
| `dd` | 行削除 |
| `yy` | 行ヤンク |
| `cc` | 行変更（削除して Insert） |
| `d/c/y` + `f/F/t/T` + `<char>` | オペレータ + 文字検索モーション |

### グローバル（全モード共通）

| キー | 動作 |
|---|---|
| `Ctrl+s` | 保存 |
| `Ctrl+t` | ファイルピッカー |
| `Ctrl+f` | 検索 |
| `Ctrl+h` | 置換 |
| `Ctrl+w` | バッファを閉じる |
| `Ctrl+q` | 終了 |
| `Ctrl+l` | レイアウト切り替え / フォーカス移動 |

### LSP・診断

| キー | 動作 |
|---|---|
| `gd` | 定義へジャンプ |
| `gi` | 実装へジャンプ |
| `gD` | 宣言へジャンプ |
| `gr` | 参照一覧 |
| `K` | ホバー情報 |
| `F2` | リネーム |
| `ed` | 診断ポップアップ |
| `ew` | 診断リスト（全種類） |
| `ee` | 診断リスト（エラーのみ） |
| `eW` / `eE` | ワークスペース診断 |

### ナビゲーション

| キー | 動作 |
|---|---|
| `b` / `B` | ジャンプ履歴（戻る / 進む） |
| `gt` / `gT` | ファイル先頭 / 末尾 |
| `gg` / `gG` | 次 / 前の Git hunk |
| `gw` / `gW` | 次 / 前の診断 |
| `ge` / `gE` | 次 / 前のエラー |
| `Ctrl+g` | 行番号入力ジャンプ |

---

## ドキュメント型

```rust
pub enum Document {
    Editable(EditableDocument),   // 通常ファイル（Rope）
    LargeFile(LargeFileDocument), // 10MB 超（読み取り専用、64KB ウィンドウ）
    Scratch(ScratchDocument),     // スクラッチバッファ（診断・検索結果表示）
}
```

`EditableDocument` の主要フィールド：
- `rope: Rope` — ropey によるテキスト本体
- `undo_stack / redo_stack: Vec<Rope>` — undo/redo 履歴
- `git_gutter_markers` — 行ごとの Git 変更状態（+/!/−）
- `diagnostics` — LSP 診断（行ごと）
- `semantic_tokens` — LSP セマンティックトークン（行ごと）

---

## UI レイアウト

### レイアウトモード

| モード | 説明 |
|---|---|
| `Single` | 単一ペイン |
| `Dual` | 左右分割（2 ドキュメント同時表示） |
| `TerminalSplit` | 左エディタ + 右ターミナル |

### 描画コンポーネント

| コンポーネント | 説明 |
|---|---|
| ドキュメントペイン | 行番号・診断マーカー（E/W）・Git gutter・テキスト |
| ステータスライン | モード / ファイル名 / 行列 / 診断サマリ（Powerline 風） |
| コマンドヒント | `<buffer [pending] \| replay [last]>` |
| ファイルピッカー | ポップアップ（72×12）、ファジーマッチ |
| 検索ポップアップ | スコープ・大小文字・正規表現・単語全体のオプションを内蔵 |
| 置換ポップアップ | From / To 2 段入力、検索オプション共有 |
| which-key ポップアップ | prefix キー待機中に画面右上へ次キー一覧を表示 |
| ホバーポップアップ | LSP hover 応答 |
| 補完メニュー | カーソル位置に最大 8 件 |
| トースト通知 | 右下、一時的 / 永続的 |
| ターミナルペイン | vt100 パーサで ANSI エスケープシーケンスをレンダリング |

---

## LSP 統合

**現在**: Rust ファイル（`.rs`）専用。rust-analyzer をバックグラウンドプロセスとして起動し、tokio 非同期で通信する。

**将来の方針**: 言語ごとに LSP サーバーを切り替えられる設計へ拡張する。ファイル拡張子でサーバー種別を判定し、設定で任意の LSP サーバーを指定できるようにする。設計上は `LspClientState` を言語非依存のインターフェースに昇格させることを想定。

対応機能：

| 機能 | キー |
|---|---|
| DidOpen / DidChange / DidSave / DidClose | （自動） |
| 診断 (publishDiagnostics) | （自動） |
| セマンティックトークン | （自動） |
| Goto Definition / Declaration / Implementation | `gd` / `gD` / `gi` |
| References | `gr` |
| Hover | `K` |
| Rename | `F2` |
| Completion | Tab（Insert モード） |
| Selection Range | `d/c/y` + `i`（構文単位選択） |

診断・セマンティックトークンはファイル修正時刻ベースでキャッシュ。

---

## シェル統合

`Ctrl+Space` でターミナルペインを開く。Unix PTY を通じて対話的シェルを起動し、vt100 クレートで出力をパースして ratatui でレンダリング。ウィンドウリサイズ時は PTY にも通知。

---

## 設定（環境変数）

| 変数 | デフォルト | 説明 |
|---|---|---|
| `MY_EDITOR_LARGE_FILE_THRESHOLD_BYTES` | 10MB | LargeFile モードの閾値 |
| `MY_EDITOR_LARGE_FILE_READ_WINDOW_BYTES` | 64KB | LargeFile の読み込みウィンドウサイズ |
| `MY_EDITOR_SHELL_PROGRAM` | `/bin/sh` | シェル統合で使用するシェル |

---

## 使用クレート

| クレート | 用途 |
|---|---|
| ratatui 0.29 | TUI レンダリング |
| crossterm 0.28 | ターミナル制御・イベント |
| ropey 1.6 | Rope テキストバッファ |
| lsp-server 0.7 / lsp-types 0.97 | LSP プロトコル |
| tokio 1.47 | 非同期ランタイム（LSP・シェル） |
| vt100 0.15 | VT100 / ANSI パーサ |
| nix 0.31 | Unix PTY 制御 |
| fuzzy-matcher 0.3 | ファイルピッカーのファジーマッチ |
| walkdir 2.5 | ディレクトリ走査 |
| clap 4.5 | CLI 引数パース |
| serde / serde_json | JSON シリアライズ（LSP 通信） |

---

## 開発状況

アーキテクチャを根本から書き直し中。`src/main.rs` はモジュール宣言のみ（`fn main() {}`）。

旧実装（`src/app/` 以下の大部分）はコメントアウトして参照用に残してある。設計を確認しながら新しいアーキテクチャへ再実装していく段階。

### モジュール構成

```
src/
├── main.rs              エントリポイント（現在空）
├── mode.rs              Mode enum
├── config.rs            環境変数による設定
├── color.rs             カラースキーム（Monokai 風）
├── error.rs             AppError 型
├── open_candidate.rs    ファイルピッカー候補（OpenBuffer / ProjectFile）
├── picker_match.rs      ファジーマッチスコアリング
├── document.rs          Document enum（Editable / LargeFile / Scratch）
├── document/
│   ├── editable.rs      EditableDocument（Rope + undo/redo）
│   ├── large_file.rs    LargeFileDocument（読み取り専用）
│   └── scratch.rs       ScratchDocument
├── app.rs               App 構造体・メインループ
└── app/
    ├── action.rs        ReplayableAction / PendingNormalAction
    ├── keymap.rs        キーマップ状態機械
    ├── render.rs        ratatui 描画ロジック
    ├── lsp.rs           LSP クライアント
    ├── semantic.rs      セマンティックトークンデコード
    ├── search.rs        検索
    ├── replace.rs       置換
    ├── completion.rs    補完
    ├── navigation.rs    ジャンプ・カーソル移動
    ├── workspace.rs     ワークスペース管理
    ├── shell.rs         シェル統合（PTY）
    └── terminal_session.rs  ターミナルセッション
```

---

## Which-key ガイダンスポップアップ（設計）

prefix キーを押した後、数十ms の遅延を経て画面右上コーナーに次キーの一覧をポップアップ表示する。

**目的**: キーシーケンスを覚えていなくても UI が次の選択肢を提示し、認知コストを下げる。熟練後は遅延中にキー入力が完了するため表示されない。

**対象となる prefix:**

| prefix | 内容 |
|---|---|
| `g` | gd/gi/gD/gr/gt/gT/gg/gG/gw/gW の一覧 |
| `e` | ed/ew/ee/eW/eE の一覧 |
| `d` / `c` / `y` | 次に取れるモーション（f/F/t/T/i など）の一覧 |

**表示位置**: 画面右上コーナー（右下はトースト通知に使用中）

**表示例 (`g` を押した場合):**
```
┌────────────────────────┐
│ g →                    │
│   d  定義へジャンプ     │
│   i  実装へジャンプ     │
│   D  宣言へジャンプ     │
│   r  参照一覧           │
│   t  ファイル先頭       │
│   T  ファイル末尾       │
│   ...                  │
└────────────────────────┘
```

**煩わしさ対策:**
- 遅延（数十ms）: 素早く入力すれば表示されない
- 背景色はエディタ背景に近い落ち着いたトーン
- 次キー入力で即消去

---

## 検索・置換機能（設計目標）

VSCode の検索機能相当の水準を目指す。

### 検索オプション

| オプション | 切替キー | 説明 |
|---|---|---|
| 大文字小文字区別 | `Alt+c` | Case Sensitive |
| 正規表現モード | `Alt+r` | Regex |
| 単語全体一致 | `Alt+w` | Whole Word |
| スコープ切替 | `Ctrl+f`（ダイアログ内） | CurrentFile / OpenBuffers / Project |

### ガイダンス UI

オプションの切替キーと現在状態をダイアログ内に常時表示（VSCode の [Aa] [.*] [W] ボタン相当）。

```
┌────────────────────────────────────────────────┐
│ Search [file|buffers|project]: _               │
│ [Alt+C] 大小区別:OFF  [Alt+R] 正規表現:OFF      │
│ [Alt+W] 単語全体:OFF  [Ctrl+f] スコープ切替     │
└────────────────────────────────────────────────┘
```

キーを覚えていなくてもダイアログを見れば操作できる。トグル状態は ON 時に強調色で表示。
