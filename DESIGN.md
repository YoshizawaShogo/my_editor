# my_editor 設計書

## 1. コンセプト

- **利用形態**: Linux サーバへ SSH した先で動かす TUI テキストエディタ。ローカルでも動くが、最適化・設計判断は常に「SSH 越し・端末上」を前提にする。
- **操作感**: VSCode ライク（非モーダル、`Ctrl+T` ファイル検索、マルチカーソル、統合ターミナル）。ただし VSCode の全機能は再現せず、必要なショートカット／機能に絞る（確定キーマップは §7.4）。
- **非対応（明示的な非目標）**: プラグイン／拡張機構は一切持たない。機能はすべてコアに内蔵する。

### 1.1 非目標（v1 で作らないもの）

- プラグイン／拡張 API。
- 複数文字エンコーディング（UTF-8 のみ）。
- grapheme cluster / 結合文字 / 絵文字幅の厳密対応（char + 表示幅近似で妥協。§6.3）。
- 3 モード以外の任意レイアウト、エディタの多分割、ターミナルの複数タブ。
- undo ツリー（分岐履歴）。
- マルチルートワークスペース。
- ミニマップ / 右端 overview ruler（TUI では作らない。画面外の提示はエッジ矢印＋件数で行う。§9.5）。
- キーバインドのユーザ再定義（キーマップは内蔵固定）。
- 行の上下移動（Move Line Up/Down）・行の複製（Duplicate Line）。使わないため作らない。
- LSP 参照検索（Find References, `textDocument/references`）。スコープを絞る方針で使用頻度が低く、問題は lint で落とすため作らない。
- LSP シンボル移動／アウトライン（`textDocument/documentSymbol`）。専用レイアウト（サイドバー等）の追加コストに見合わず、`Ctrl+F` 検索で代替できるため作らない。
- LSP コードアクション／クイックフィックス（`textDocument/codeAction`）。使わないため作らない。修正は手動＋lintで対応。
- 名前を付けて保存（Save As）。使わないため作らない。新規ファイルはファイルピッカーへのパス直接入力で作成する。
- 保存時フォーマット（format on save）・保存時末尾空白除去。使わないため作らない。整形は必要時に手動で行う。
- ファイル監視（inotify 等）による外部変更の即時検知。競合ケースはレアで、かつ実行前に競合すると予見できるため、数秒間隔のポーリング（§5.x の `CheckDiskStates`）＋ `F5` 手動再読込で十分。
- LSP サーバー個別設定（`initializationOptions` / workspace 設定）。サーバーをカスタムして使う需要は稀で、既定設定で運用するため作らない。
- インレイヒント（LSP `inlayHint`）。仮想テキストが編集の妨げになり（その位置を編集できない・カーソルが飛ぶ）、体験を損なうため作らない。
- 全一致の一括選択（select all occurrences）。置換で代替できるため作らない。`Ctrl+D`（次の一致を1つずつ追加）は維持する。
- 構文単位の選択拡大／縮小（expand/shrink selection）。エディタ機能としては不要（作らない）。
- 行の連結（Join Lines）。使わないため作らない。

## 2. 設計原則（実装全体の指針）

この 5 原則を全モジュールで守る。判断に迷ったらこれに従う。

1. **テスト容易性を最優先**。状態遷移を純粋関数に閉じ込め、IO と分離する（§3）。
2. **スコープ最小化**。型には拡張の余地を残しつつ、実装は必要最小限から始める（例: マルチカーソルは型は複数・初期実装も複数だが、分割は 3 モード固定）。
3. **抽象化は具体的な理由があるときだけ**。ペインレイアウトは trait でなく 3 変種の enum。
4. **型は具体的な enum variant で構造を表現し、各型は独立に定義して `From` で接続する**。`AppEvent` / `Effect` / `Overlay` / `Highlighter` はすべてこの方針。
5. **関数シグネチャは実際に使う引数だけ受け取る**。公開範囲（`pub`）の調整はリファクタリングの最後に行う。

## 3. アーキテクチャ

### 3.1 全体データフロー

tokio 中心の非同期。ただし **エディタ状態はメインループが単一所有** し、スレッド間共有しない（`Arc<Mutex>` を使わない）。重い IO（LSP・ターミナル・ファイル走査・grep）は独立した tokio タスク／スレッド＝**アクター**に切り出し、`channel` で `AppEvent` をやり取りする。

```
                        ┌──────────────────────────────────────────┐
   端末入力(key/mouse)   │            メインループ (単一所有)          │
   ───────────────►     │                                          │
                        │  RawInput ─(keymap翻訳: focus依存)─► AppEvent
   LSP応答  ─┐          │                                          │
   PTY出力  ─┤ AppEvent │        AppEvent ──► update(&mut Editor)   │
   grepヒット─┤ (mpsc)   │                        │                 │
   走査結果 ─┘  ────────►│                        ▼                 │
                        │                   Vec<Effect>            │
                        │                        │                 │
                        │            scheduler: 各Effectをspawn     │
                        └────────────┬─────────────────────────────┘
                                     │ Effect(IO要求)
                     ┌───────────────┼───────────────┬───────────────┐
                     ▼               ▼               ▼               ▼
                 LSPアクター     ターミナル       ファイルIO        grep/走査
                 (子プロセス)     アクター(PTY)                    アクター
                     │               │               │               │
                     └───────────────┴───────────────┴───────────────┘
                                完了は AppEvent として mpsc へ戻る
```

要点:

- **2 段階入力**: 生入力 `RawInput` を、focus に応じて意味的な `AppEvent` に翻訳してから `update` に渡す。keymap は「(focus, key) → AppEvent」の純粋な写像。
- **状態遷移は純粋関数**: `Editor::update(&mut self, ev: AppEvent) -> Vec<Effect>`。IO を一切呼ばず、必要な IO を `Effect` として返すだけ。
- **Effect 分離**: scheduler が `Effect` を tokio タスクとして実行し、その完了を新たな `AppEvent` として mpsc に戻す。
- **単一 FIFO キュー**: すべての `AppEvent` を単一 `mpsc::UnboundedReceiver` で受ける。優先度制御はしない。
- **描画はメッセージ駆動＋合成**: 1 イベント処理後、キューに溜まっている分を `try_recv` で drain してまとめて `update` し、`dirty` なら最後に 1 回だけ描画する（SSH 回線での無駄な再描画を避ける）。

### 3.2 ループ骨格

```rust
// runtime/mod.rs（シグネチャは最小引数の方針で最終調整）
struct Runtime {
    editor: Editor,
    tx: mpsc::UnboundedSender<AppEvent>,      // アクター/入力タスクへ配布
    rx: mpsc::UnboundedReceiver<AppEvent>,
    lsp: HashMap<ServerId, LspHandle>,        // 各LSPアクターへの送信口
    terminal: Option<TerminalHandle>,         // PTYアクターへの送信口
    term: ratatui::Terminal<...>,             // 描画バックエンド
}

impl Runtime {
    async fn run(&mut self) -> Result<()> {
        self.spawn_input_reader();            // crossterm購読 → 翻訳 → AppEvent
        self.draw()?;                         // 初回描画
        while let Some(ev) = self.rx.recv().await {
            let mut effects = self.editor.update(ev);
            while let Ok(ev) = self.rx.try_recv() {     // drain して描画を合成
                effects.extend(self.editor.update(ev));
            }
            for eff in effects {
                self.execute(eff).await;       // spawn 中心。即時失敗は AppEvent::Error へ
            }
            if self.editor.take_dirty() {
                self.draw()?;
            }
            if self.editor.quit {
                break;
            }
        }
        Ok(())
    }
}
```

- 入力購読は `crossterm` の `EventStream`（`event-stream` フィーチャ）を使う独立タスク。翻訳して `AppEvent` を送る。
- `execute` は基本 `tokio::spawn`。走査/grep/LSP のような長時間 IO はアクター側で継続し、結果を分割して `AppEvent` で戻す。

## 4. コアデータモデル

型は独立定義し `From` で接続する（原則 4）。以下はシグネチャのスケッチで、最終的な `pub` 範囲・引数は実装時に最小化する。

### 4.1 Editor（ルート状態）

```rust
struct Editor {
    documents: HashMap<DocumentId, Document>,
    next_doc_id: u64,
    layout: Layout,                // 3モード固定（§4.4）
    focus: Focus,                  // どのペイン/オーバーレイが入力を持つか
    overlay: Option<Overlay>,      // フローティング補助UI（§4.6）
    lsp: LspRegistry,              // サーバ管理・request対応付け（§8）
    clipboard: Register,           // 内部レジスタ（§10）
    config: Config,                // 言語判定・LSPコマンド等（§11）
    workspace_root: PathBuf,
    status: StatusMessage,         // ステータスバー1行
    notifications: Notifications,  // 右下トースト＋進捗（§4.10）
    pending_keys: KeyChordState,   // chord進行中の状態（§7）
    dirty: bool,                   // 再描画要求
    quit: bool,
}

struct DocumentId(u64);
```

`update` はカテゴリごとにサブディスパッチする（§5）。

### 4.2 Document（バッファ）

閾値を超える巨大ファイルでフリーズしないよう、バッファ内容は **編集可能（Rope）** と **大容量読み取り専用（mmap ページング）** の 2 種に分ける（具体 enum。§4.2.1）。共通メタデータは `Document` に置き、編集専用の状態は `Editable` に閉じ込める。

```rust
struct Document {
    path: Option<PathBuf>,         // 無名バッファは None
    language: Option<LanguageId>,  // 言語判定結果（§11.2）
    kind: DocumentKind,
}

enum DocumentKind {
    Editable(Editable),            // 通常。Rope・履歴・LSP・ハイライトあり
    Large(LargeFile),              // 閾値超え。読み取り専用・syntax/LSP なし（§4.2.1）
}

struct Editable {
    text: Rope,                    // ropey
    line_ending: LineEnding,       // 読込時に検出し保存時に復元
    version: i32,                  // LSP のドキュメントバージョン
    modified: bool,
    history: History,              // undo/redo（§4.5）
    diagnostics: Vec<Diagnostic>,  // LSP publishDiagnostics 由来（ガター診断列）
    git_gutter: GitGutter,         // HEAD との行差分（ガター git 列。§9.4）
    highlighter: Highlighter,      // 着色元（§9）
}

enum LineEnding { Lf, Crlf }
```

### 4.2.1 大きなファイルの扱い（フリーズ回避）

サイズが閾値（`config.editor.large_file_threshold`、既定 10 MiB 程度）を超えるファイルは、Rope へ全読み込みせず **less 相当のページングビュー**で開く。**syntax ハイライトも LSP も一切適用しない**。

```rust
struct LargeFile {
    mmap: Mmap,          // memmap2。ファイルをメモリマップし、ヒープに全読込しない
    index: LineIndex,    // 行開始バイトオフセットを遅延構築
    read_only: bool,     // 常に true（編集コマンドは受け付けない）
}

struct LineIndex {
    starts: Vec<u64>,    // 既知の行開始オフセット（走査済みぶんのみ）
    scanned_to: u64,     // ここまで改行走査済み
    complete: bool,      // 末尾まで走査済みか
}
```

- **メモリを消費しない読み方**: `memmap2` でファイルをマップし、**表示中の行範囲のバイトだけ**をその都度読んで UTF-8 デコード（不正バイトは lossy 表示）。全文字列を確保しない。行インデックスはスクロールで前進するたびに `starts` を追記する遅延構築（less と同様、訪れた範囲だけ既知になる）。末尾ジャンプ・パーセント移動は末尾から後方走査で対応。
- **判定と分岐**: ファイルオープンは `Effect::ReadFile` の実行時に**まずサイズを stat** し、`> large_file_threshold` なら mmap で `DocumentKind::Large` を、以下なら通常読み込みで `Editable` を構築して `AppEvent::Io` で返す（閾値は `config` から。判定 IO はランタイム側、`update` は純粋のまま）。極端に長い 1 行（minified 等）も基本サイズが大きく Large 経路に入るため、tree-sitter/LSP による硬直を避けられる。
- **無効化される機能**: `Large` では tree-sitter・LSP（`didOpen` を送らない）・undo/履歴永続化・編集コマンド・整形を行わない。カーソル表示とコピー、スクロール、単一ファイル検索のみ。
- **検索**: `Large` に対する `Ctrl+F` は、メモリ内マッチではなく**その 1 ファイルへのストリーミング grep**（`grep` クレート、§13）にフォールバックし、ヒット行へジャンプする。
- **リスク注記**: mmap 中に外部からファイルが truncate されると SIGBUS の可能性がある。読み取り専用ビューでは許容とし、P10 で必要ならシグナルハンドリング／seek 読みフォールバックを検討（`memmap2` は高信頼だが要留意）。

### 4.2.2 バイナリ / 非 UTF-8 の検出

UTF-8 のみ対応のため、バイナリや別エンコードを開くと文字化け・破綻する。オープン時に**先頭チャンク（例 8KiB）を検査**し、`NUL` バイトを含む／UTF-8 として不正な場合は**バイナリとみなして開かない**。`notify(Warn, "バイナリ/非UTF-8のため開けません: <name>")` を出すのみ（§4.10）。判定は stat と同じくオープン IO 内（ランタイム側）で行い、`update` は結果 `AppEvent::Io` を受けるだけ。

### 4.3 View / EditorPane

```rust
struct EditorPane {
    view: View,
}

struct View {
    doc: DocumentId,
    selections: Selections,        // 非空を不変条件に保つ（§4.7）
    scroll: Scroll,                // 表示上端の論理行・先頭行内の折り返し表示行
}

struct Scroll { top_line: usize, wrapped_row_offset: usize }
```

通常Documentの長い論理行はペイン幅でsoft wrapする。ガターと本文を別領域として描画し、継続行は行番号・診断・gitのガターを貫通させず本文領域内だけで折り返す。折り返し後の表示行をカーソル可視化・マウスhit-test・スクロールでも同じ規則で数える。

### 4.4 Layout（3 モード固定の enum）

trait 抽象化はしない。variant で構造を表す（原則 3・4）。

```rust
enum Layout {
    EditorFull(EditorPane),
    EditorAndShell { editor: EditorPane, shell: TerminalPane },
    EditorAndEditor { left: EditorPane, right: EditorPane },
}
```

- モード 1: エディタ全画面。
- モード 2: 左エディタ＋右シェル（統合ターミナル、単一セッション）。
- モード 3: 左右ともエディタ。
- これ以外のレイアウトは考慮しない。
- 左右分割では中央の1セルを専用の境界列として確保し、縦罫線`│`を描画する。左右ペインの本文・PTY・マウス座標はこの1セルを除いた幅で計算する。

### 4.5 History（線形 undo ＋グルーピング）

```rust
struct History {
    past: Vec<Revision>,
    future: Vec<Revision>,
    pending: Option<Revision>,     // グルーピング中の編集をまとめる
}

struct Revision {
    changes: Vec<Change>,          // 適用順。undo は逆順で逆適用
    selections_before: Selections,
    selections_after: Selections,
}

struct Change {
    range: Range<CharIdx>,         // 置換対象（char index）
    removed: String,               // 逆適用に使う旧テキスト
    inserted: String,
}
```

- 連続入力・同種編集・短時間内は `pending` に coalesce し、区切り（カーソルジャンプ・別種操作・タイムアウト・保存）で確定して `past` へ。
- 新規編集で `future` はクリア（分岐は持たない）。
- `Ctrl+S`は履歴を消さず、その時点の内容ハッシュを保存タグとして更新し、連続入力グループだけを区切る。undo/redo後の内容ハッシュが保存タグと一致する時だけ「保存不要」と判定する。

### 4.5.1 undo 履歴の寿命

undo/redo履歴は各バッファのメモリ上だけに保持する。バッファを閉じた時、またはエディタを終了した時に破棄し、ディスクへの書き出しと再オープン時の復元は行わない。

### 4.6 Overlay（フローティング補助 UI、具体 enum）

パネル／サイドバーを持たないため、補助UIはエディタ上に浮かぶオーバーレイとして描画する。Picker/Search/Rename/Confirmは開いている間focusを奪う。Completionは専用focusを使い、上下・Enter・Ctrl+C・Esc・Ctrl+@だけを候補操作として扱い、それ以外は通常の編集操作としてバッファへ渡す。**Hoverはfocusを奪わない**（通常クリックで入力カーソルを移した位置に表示し、LSP hover情報とその位置の診断本文を集約＝§9.6）。MouseMovedや範囲選択中は表示しない。

```rust
enum Overlay {
    Picker(PickerState),           // Ctrl+T ディレクトリ以下 / Ctrl+G 全バッファ / F6 比較対象（§13）
    Search(SearchState),           // Ctrl+F 検索 / Ctrl+H 置換。スコープ循環（§13）
    Completion(CompletionState),   // Ctrl+@（Ctrl+Space）トグル。矢印選択/Enter・Tab確定（§8.5）
    Hover(HoverState),             // 通常クリック位置のLSPホバー＋診断本文（focusを奪わない。§9.6）
    Rename(RenameState),           // LSPリネーム入力
    Confirm(ConfirmState),         // 未保存終了などの確認
}
```

### 4.7 Selection / Selections（マルチカーソル）

マルチカーソルは最初から実装する。

```rust
struct Selections {
    ranges: Vec<Selection>,        // 常に1件以上（不変条件）
    primary: usize,                // ranges 内のインデックス
}

struct Selection {
    anchor: CharIdx,               // 選択の固定端
    head: CharIdx,                 // キャレット位置。anchor==head でキャレットのみ
}
```

- すべての編集・移動コマンドは `ranges` 全件に作用する。
- 編集適用時は、前方の変更が後方カーソルの char index をずらすためオフセット補正を行う（§5.2）。
- 重なった選択はマージ、`Esc` で primary のみへ collapse。

### 4.8 Focus

```rust
enum Focus {
    Editor(Side),   // 単一ペイン時は Left
    Shell,
    Overlay,        // overlay: Some(..) のとき
}
enum Side { Left, Right }
```

### 4.9 コマンド発見（認知コスト低減）

独立したヒントガイドは持たない。`Ctrl+P`のコマンドパレット（§13）でコマンド名・説明・固定キーバインドを同時表示し、検索からそのまま実行できるようにする。操作のたびにキーを確認できるため、常設ガイドを増やさず利用しながら覚えられる。

### 4.10 Notifications（ステータスバーへ集約）

右下トーストは本文・ステータスバーと重なるため描画しない。一過性の保存結果やエラーはステータスバーへ、LSPの継続状態は言語セグメントへ集約する。内部のToastは有効期限管理に利用してよいが、独立popupにはしない。

### 4.11 LSP 進捗表示

LSP は起動〜インデックス作成に時間がかかるため、状態をステータスバーの言語セグメントへ表示する。ソースはライフサイクルイベントと`$/progress`:

1. **自前ライフサイクル段階**（`SpawnLspServer` → プロセス起動 → `initialize` 送信 → `initialized` 受信 → Ready）。各段で `Editor::progress(key=Server(id), …)` を更新（"起動中" → "初期化中" → "準備完了"）。
2. **LSP の `$/progress`（WorkDoneProgress）**: `window/workDoneProgress/create` ＋ `$/progress`(Begin/Report/End)。rust-analyzer 等が送る "Indexing" / "Loading" の title・message・percentage をそのまま `Progress` にマップ（`key=WorkDone(token)`）。

- Ready 到達・`End` 受信で `done=true`、少し残してから除去。失敗（プロセス異常終了・initialize タイムアウト）は `notify(Error, …)`。
- これにより「rust-analyzer 起動中… / Indexing 1234/5000 / 準備完了」が右下で刻々と見える。

### 4.12 スタートページ

起動時に**ファイル引数が無い／ディレクトリ引数のみ**の場合、開いているバッファが無いのでエディタ領域に**スタートページ**を出す（VSCode の welcome の極小版）。案内は最小限:

- `Ctrl+T` … ファイルを開く
- `Ctrl+P` … コマンドとキーバインドを検索
- `F4` … 終了

バッファを 1 つでも開くと消える。フォーカスは editor。`main.rs` は引数がファイルならそれを開き、ディレクトリ／無指定ならスタートページを表示する（`workspace_root` は §11.3 で決定）。

## 5. 状態遷移（update）

### 5.1 AppEvent（意味的イベント、具体 enum）

`AppEvent` は「ユーザ意図（keymap/mouse/パレット由来）」と「IO 完了（アクター由来）」の 2 系統を持つ。サブシステムのイベント型は独立定義し `From` で接続。

```rust
enum AppEvent {
    // ユーザ意図
    Command(Command),              // 高レベルコマンド（§5.3）
    TextInput(char),               // オーバーレイのテキスト入力など、焦点依存の生文字
    Mouse(MouseEvent),             // クリック/ドラッグ/ホイール（§7.3）
    Resize { cols: u16, rows: u16 },

    // IO完了（アクターから mpsc 経由で戻る）
    Lsp(LspEvent),                 // §8
    Terminal(TerminalEvent),       // §12
    Grep(GrepEvent),               // §13
    FileScan(FileScanEvent),       // finder用の走査結果
    Git(GitEvent),                 // ガター差分・branch・porcelain状態（§9.4）
    Io(IoEvent),                   // ファイル読み書き完了/失敗
    ConfigLoaded(Result<Config, ConfigError>),

    Tick,                          // カーソル点滅等（必要時のみ発行）
    Error(String),                 // Effect実行の即時失敗をステータスへ
}
```

### 5.2 update のディスパッチ

```rust
impl Editor {
    fn update(&mut self, ev: AppEvent) -> Vec<Effect> {
        let effects = match ev {
            AppEvent::Command(c)  => self.apply_command(c),
            AppEvent::TextInput(ch) => self.apply_text_input(ch),
            AppEvent::Mouse(m)    => self.apply_mouse(m),
            AppEvent::Lsp(e)      => self.apply_lsp(e),
            AppEvent::Terminal(e) => self.apply_terminal(e),
            AppEvent::Grep(e)     => self.apply_grep(e),
            AppEvent::FileScan(e) => self.apply_file_scan(e),
            AppEvent::Git(e)      => self.apply_git(e),
            AppEvent::Io(e)       => self.apply_io(e),
            AppEvent::Resize { .. } | AppEvent::Tick
            | AppEvent::ConfigLoaded(_) | AppEvent::Error(_) => { /* ... */ }
        };
        self.mark_dirty_if_needed();
        effects
    }
}
```

- 純粋（同期・IO なし）。テストは「`AppEvent` 列を流して state と返り値 `Vec<Effect>` を assert」で行う。
- 編集系はまず `Change` を生成 → `Document::apply`（rope 更新＋history 記録）→ 全カーソルのオフセット補正 → LSP へ `didChange` を送る `Effect` を返す、という流れに統一する。

### 5.3 Command（高レベルコマンド）

keymap（§7.4 で確定）が生成する単位。生成元はkeymap、マウス、`Ctrl+P`のコマンドパレット。以下は主なもの:

```rust
enum Command {
    // 編集
    InsertNewline, DeleteBackward, DeleteForward, DeleteWordBackward,
    Indent, Outdent, Undo, Redo,
    // カーソル/選択
    Move { dir: Direction, unit: Unit, extend: bool },  // 文字/単語/行/文書
    SelectAll, CollapseSelections,
    AddCursor { dir: VerticalDir }, SelectNextOccurrence,
    // クリップボード
    Copy, Cut, Paste,
    // ファイル/バッファ（ピッカー。§13）
    Save, CloseBuffer, OpenDirectoryPicker, OpenBufferPicker, OpenDiffPicker,
    OpenCommandPalette,
    // 検索（スコープ循環。§13）
    OpenSearch, OpenReplace, OpenSearchInDirectory, CycleSearchScope,
    // 編集(言語補助)
    ToggleComment,
    // レイアウト（フォーカスは操作に追従。専用フォーカスコマンドは持たない）
    ToggleSplit, ToggleShell,
    // LSP（定義=Ctrl+クリック / ホバー=自動 は Command でなくイベント経由）
    Rename, ToggleCompletion,
    // 終了
    Quit,
}
```

### 5.4 Effect（外向き IO 要求、具体 enum）

`update` はこれを返すだけで IO を実行しない。scheduler が実行し、完了を `AppEvent` で戻す。

```rust
enum Effect {
    // ファイルIO
    ReadFile { id: DocumentId, path: PathBuf },
    WriteFile { path: PathBuf, contents: String, doc: DocumentId },

    // 走査・検索（token で古いクエリの結果を捨てる）
    StartFileScan { root: PathBuf, token: ScanToken },
    StartGrep { query: GrepQuery, root: PathBuf, token: GrepToken },

    // git 差分（ガター。§9.4）
    ComputeGitStatus { doc: DocumentId, path: PathBuf },

    // LSP
    SpawnLspServer { language: LanguageId, command: LspCommand, root: PathBuf },
    LspNotify { server: ServerId, msg: LspOutbound },
    LspRequest { server: ServerId, msg: LspOutbound, correlation: ReqToken },

    // ターミナル
    SpawnShell { size: (u16, u16) },
    TerminalInput { bytes: Vec<u8> },
    TerminalResize { size: (u16, u16) },

    // クリップボード（手元端末へ）
    ClipboardOsc52(String),

    // 制御
    LoadConfig,
    Quit,
}
```

### 5.5 編集補助（オートインデント・単語境界・括弧対応）

VSCode ライクの操作感に必要な最小の編集補助:

- **オートインデント**: `Enter`でカーソル位置までの先頭空白を引き継ぐ。インデント途中で分割しても後半の既存空白を重複させない。現在行が言語設定の`line_comment`（`//`/`#`等）なら、同じインデント＋コメント記号＋空白を次行へ継続する。`///`/`//!`も維持する。
- **Tab/Shift+Tab/Backspace**: 選択がない場合、Tabは各カーソル位置へ`tab_size`個のspace（`insert_spaces=false`なら実タブ1文字）を挿入し、カーソルを挿入後へ進める。Backspaceは直前がspaceならカーソル表示列が`tab_size`の倍数へ戻るまで削除する。範囲選択中は対象行をまとめて行頭インデントする。Shift+Tabは各行に実在する先頭spaceを最大`tab_size`個、または先頭タブ1文字だけ削除し、文字本文へ食い込まないようクランプする。
- **単語境界の定義**: 識別子文字（英数字 ＋ `_`）を 1 単語とする定義を `view/movement.rs` に一元化し、`Ctrl+←/→`（単語移動）・`Ctrl+D`（次の同一語）・ダブルクリック（単語選択）で共有する。
- **入力ペア**: `()`、`[]`、`{}`、single quote、double quote、backtickを開始文字の入力時に補完してペア内へカーソルを置く。閉じ文字の直前で同じ閉じ文字を入力した場合は挿入せず追い越す。空ペア内のBackspaceは両方を1操作で削除する。`()`・`[]`・`{}`の空ペア内でEnterを押すと2回改行し、内側を1段深く、閉じ括弧を元の行インデントへ揃える。
- **閉じ波括弧**: 行頭空白だけの位置で`}`を入力した場合、対応する`{`の行頭インデントへ揃えてから挿入する。
- **括弧の対応ハイライト**: カーソル隣接の `()[]{}` に対応する括弧を探して両方を強調表示。
- **同一語ハイライト**: カーソル隣接の識別子と完全一致する単語を、現在の描画範囲内だけ検索して淡い背景で表示する。
- **マウス**（§7.3 に対応）: ダブルクリック＝単語選択、トリプルクリック＝行選択。

## 6. 位置表現と変換層

### 6.1 3 つの座標

境界に変換を集約し、内部は char index に統一する（純粋関数でテスト）。

```rust
struct CharIdx(usize);              // rope上のchar index（内部の基軸）
struct DisplayPos { line: usize, col: usize }   // 表示座標。col は表示幅（全角=2, タブ=tab_size）
// LSP座標は lsp_types::Position（line, utf-16 code unit）を境界でのみ使う
```

### 6.2 変換関数（`position` モジュール）

- `char_idx ↔ (line, char-col)`（ropey の行 API を利用）。
- `(line, char-col) ↔ DisplayPos`（`unicode-width` で表示幅計算、タブ展開）。
- `char_idx ↔ lsp Position`（utf-16 換算）。
- これらは Rope（または対象行の文字列）に対する純粋関数として実装しテストする。

### 6.3 幅の妥協点（v1）

- 表示幅は `unicode-width` の East Asian Width 近似で計算する（全角 2）。
- grapheme cluster・結合文字・絵文字 ZWJ 連結は 1 char 単位で扱い、厳密なクラスタ境界は追わない（非目標）。カーソルは char 単位で動く。

## 7. 入力とキーバインド

### 7.1 翻訳層（RawInput → AppEvent）

keymap は内蔵固定（ユーザ再定義なし）。翻訳は focus に依存する純粋関数。

```rust
// input/translate.rs
fn translate(
    raw: RawInput,
    focus: &Focus,
    pending: &mut KeyChordState,   // chord進行状態のみ可変
    keymap: &Keymap,
) -> Option<AppEvent>
```

- **Editor focus**: 印字文字 → 編集コマンドに落とす。ショートカットは keymap 参照。
- **Overlay focus**: 印字文字 → `TextInput(ch)`（クエリ入力）、`Enter`/`Esc`/矢印などは overlay 用コマンド。
- **Shell focus**: ほぼ全キーをバイト列に変換して `Effect::TerminalInput`（一部のグローバルキーだけ横取り、例: レイアウト切替）。
- **外部ペースト**: 起動中はbracketed paste modeを有効にし、`Event::Paste`を1回の`TextPaste`として扱う。Editorでは改行や空白を加工せず一括挿入し、CRLFだけLFへ正規化する。Shellでは貼り付け文字列をPTYへ送る。

### 7.2 chord（VSCode の 2 ストローク）

```rust
struct KeyChordState { prefix: Option<KeyPress> }   // 例: Ctrl+K 押下後
```

`Ctrl+K` などの prefix を受けたら `pending` に保持し、次キーで確定。タイムアウト・不一致で破棄。ただし **MVP の確定キーマップ（§7.4）には chord 割当が無い**ため、`KeyChordState` は将来用の最小スタブに留める（実装は後回し可）。

### 7.3 マウス（基本対応）

`crossterm` のマウスイベントを `AppEvent::Mouse` に翻訳。右手のマウスが操作の主役で、フォーカス・スクロール・定義ジャンプ・範囲選択を担う（§7.4 確定）:

- クリック: そのペインにフォーカス＋カーソル移動（char 位置へ hit-test）。
- ドラッグ: 選択範囲を拡張。
- ホイール: スクロール（キーボードのページ送りは持たない）。
- Ctrl+クリック: 定義ジャンプ（LSP。専用キーは無い）。
- Alt+クリック: カーソル追加（マルチカーソル）。

### 7.4 採用ショートカット（確定）

VSCode ライクでも全ショートカットは実装しない。**右手はマウス**で範囲選択・スクロール・フォーカス・定義ジャンプを担い、**キーボードは左手中心の最小セット**に絞る。専用のフォーカス移動キーは持たず、フォーカスは操作（分割/シェル/ピッカー/クリック）に追従する。

#### マウス（右手）

| 操作 | 動作 |
|------|------|
| クリック | ペインにフォーカス＋カーソル移動（char へ hit-test） |
| ドラッグ | 範囲選択 |
| ホイール | スクロール（キーボードのページ送りは持たない） |
| Ctrl+クリック | 定義ジャンプ（LSP。専用キーは無い） |
| Alt+クリック | カーソル追加（マルチカーソル） |

#### キーボード（左手中心・確定）

| キー | Command | 備考 |
|------|---------|------|
| 印字キー | 文字挿入（全カーソル） | |
| `Enter` / `Backspace` / `Delete` | 改行 / 前削除 / 後削除 | |
| 矢印 / `Home` / `End` | カーソル移動 | `Home` はスマートHome（行頭の非空白↔0桁をトグル）。`Ctrl+Home/End` は文書頭/末 |
| `Ctrl+←/→` | 単語移動 | |
| `Ctrl+Home` / `Ctrl+End` | 文書頭 / 末へ | ページ送りの代替 |
| `Ctrl+N` | 行番号ジャンプ（Go to Line） | 番号入力プロンプトを開き、指定行へ移動。`Ctrl+G` はバッファピッカーで使用済みのため別キー。`Ctrl+J` は Enter(LF)衝突、`Ctrl+L` は clear の癖で誤爆するため避けた |
| `Shift+移動` | 選択拡張 | |
| `Ctrl+A` | 全選択 | |
| `Ctrl+D` | 次の同一語を選択 | マルチカーソル |
| `Ctrl+Alt+↑/↓` | 上下にカーソル追加 | マルチカーソル |
| `Tab` / `Shift+Tab` | インデント / アンインデント | 選択行 |
| `Ctrl+/` , `Ctrl+Q` | コメントトグル | **2 キー割当**。右手マウスで範囲選択中に左手でコメントできるよう `Ctrl+Q` を併設 |
| `Ctrl+Z` / `Ctrl+Y` | undo / redo | |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | コピー / カット / ペースト | 内部＋OSC52。選択なしの`Ctrl+C`は現在行をlinewiseコピー |
| `Ctrl+S` | 保存 | |
| `Ctrl+W` | バッファを閉じる | 未保存なら `Confirm`（§4.6） |
| `Ctrl+T` | ファイルピッカー（カレントディレクトリ以下） | 再押下で閉じる。`ignore`走査。`Ctrl+P`はコマンドパレットに使用 |
| `Ctrl+P` | コマンドパレット | 名前・説明・キーをfuzzy検索。候補にキーバインドを併記し、`Enter`で実行 |
| `Ctrl+G` | バッファピッカー（開いている全バッファ） | 再押下で閉じる |
| `F6` | 比較対象バッファピッカー | 現在のバッファを左、選択した開いているバッファを右に置き、行を対応させた diff 表示 |
| `Ctrl+F` / `Ctrl+H` | 検索 / 置換（オーバーレイを開く。**開いた状態で再押下するとスコープを循環**。include/exclude glob と ignore 設定あり＝§13） | カレントバッファ→全バッファ→カレントディレクトリ以下 |
| `Ctrl+Shift+F` | 検索をディレクトリスコープで直接開く | 循環の 3 段目へ一発 |
| `Ctrl+@` / `Ctrl+Space` | 補完ポップアップ トグル | 同じNUL入力を同一操作として扱う。上下で選択 / `Enter`で確定。表示中も他のキーは通常編集 |
| `F2` | リネーム | LSP |
| `F5` | ファイル再読込 | ディスクから読み直す。未編集は自動再読込済み（§4.x）だが、未保存編集ありの競合時は確認ダイアログで破棄可否を尋ねる |
| `Ctrl+]` | 左右エディタ分割トグル | フォーカス追従（モード1↔3） |
| `Ctrl+O` | シェル表示トグル | フォーカス追従（モード1↔2） |
| `F4` | エディタ終了 | Alt+F4 のイメージ。未保存があれば `Confirm`。あまり押さない前提の隔離キー |
| `Esc` | オーバーレイを閉じる / 選択を単一化 | |

#### 不採用・自動化（明示）

- `PageUp` / `PageDown`: **不採用**（マウスホイールで代替）。
- 専用フォーカス移動キー（`Ctrl+1/2`等）: **不採用**。フォーカスはマウス / `Ctrl+]` / `Ctrl+O` / `Ctrl+T` / `Ctrl+G`に追従。
- `Ctrl+Shift+P`: 不採用。コマンドパレットは左手で押しやすい`Ctrl+P`に固定する。
- 定義ジャンプ: **Ctrl+クリックのみ**（`F12` は持たない）。
- ホバー: **キー無し。通常クリックで入力カーソルを移した位置に表示**。MouseMovedでは表示しない。
- 整形: **MVP は導線なし**（機能は後続 P8。パレット導入時に割当）。

コメントトグルは言語ごとの行コメント記号を要するため、`[[language]]` 設定に `line_comment` を持たせる（§11.2 の拡張余地）。バンドル言語には既定値を内蔵。

## 8. LSP サブシステム

### 8.1 方針

- `lsp-server`（rust-analyzer 製・高信頼）の `Message::read`/`Message::write` を **子プロセスの stdout/stdin** に対して使い、Content-Length フレーミングを自前実装しない。
- `lsp-types` でメッセージ型。内部型との相互変換は `lsp/convert.rs` に集約。
- `lsp-server` の IO は同期ブロッキングなので、**サーバごとに読み書き 2 スレッド**を立て、tokio 側とは `mpsc` で橋渡しする（tokio 中心方針を保ちつつ状態はメインループ単一所有のまま）。

### 8.2 アクター構成

```rust
struct LspRegistry {
    servers: HashMap<ServerId, ServerState>,
    by_language: HashMap<LanguageId, ServerId>,
    pending: HashMap<RequestId, PendingRequest>,   // request対応付け（メインループ側）
    next_req: i64,
}

struct LspHandle { to_server: mpsc::Sender<LspOutbound> }   // メイン→アクター
```

- **リーダースレッド**: `Message::read(child.stdout)` をループし、`AppEvent::Lsp(LspEvent::…)` を mpsc へ送る（応答・通知・診断）。
- **ライタースレッド**: `LspOutbound` を受けて `Message::write(child.stdin)`。
- **request 対応付け**は `pending`（メインループ側の `Editor`）で管理。応答は `AppEvent::Lsp(Response{id, result})` として戻り、`update` が `pending` を引いて意図（定義ジャンプ/補完/…）に応じた後続 `AppEvent`/`Effect` を生成する。→ IO はスレッド、判断は純粋 `update`。

### 8.3 対応機能（全機能）

診断（publishDiagnostics）／補完（completion＋ポップアップ）／定義ジャンプ（definition）／ホバー（hover）／リネーム（rename＋WorkspaceEdit 適用）／整形（formatting）／シグネチャヘルプ（signatureHelp）。ドキュメント同期は `didOpen`/`didChange`(増分)/`didSave`/`didClose`。

シグネチャヘルプ（引数ヒント。`textDocument/signatureHelp`）は、関数呼び出しの入力中に引数の並びと現在位置を提示する。挙動:

- **トリガ**: サーバが `initialize` で返す `signatureHelpProvider.triggerCharacters`（無指定時は `(` と `,`）を入力したとき自動要求。表示中はさらに入力するたび再要求して `activeParameter` を追従させ、`)`・`Esc`・カーソル移動・クリックで閉じる。専用キーは持たない。
- **表示**: 関数シグネチャ1行を補完ポップアップと同じくキャレット直下に出し、`activeParameter` の引数を太字強調（残りは淡色）。複数オーバーロードは `activeSignature` を採用。パラメータ位置は LSP の UTF-16 オフセットをバイトオフセットへ変換して求める。
- **マルチカーソル時**: primary カーソルのみ（他の位置依存 LSP と同じ方針）。
- 実装は `Overlay` の variant ではなく、hover と同じく `Editor` の `signature_help: Option<..>` フィールドとして持ち、focus を奪わず描画する（トリガ文字は `LspServer` に保持）。

MVP の起動導線（§7.4 確定）:

- 定義ジャンプ: **Ctrl+クリック**（キー無し）。
- ホバー: **通常クリック時**（移動後が単一caretの時だけ`Overlay::Hover`を表示）。ドラッグ・ダブル/トリプルクリック・Shift移動等で範囲選択になったら既存Hoverも閉じる。
- リネーム: **F2**。補完: **Ctrl+@**（`Ctrl+Space`と同一、トグル。§8.5）。
- **整形は MVP では導線なし**（機能実装は P8、パレット導入時に割当）。
- **マルチカーソル時のスコープ**: 補完・定義ジャンプ・ホバー・リネームなど位置依存の LSP 操作は、MVP では **primary カーソルのみ**に適用する（実装量を絞る）。全カーソルへの一括適用は後続。
- **診断のズレ（stale diagnostics）**: 編集するとサーバ再送までの間、`diagnostics` の位置が実際とズレる。厳密追従はしないが、**編集の行デルタ分だけ暫定シフト**し、**編集された行の診断は stale として淡色化**する。次の publishDiagnostics で丸ごと置換。列レベルの厳密再マップはしない（best-effort、費用対効果で割り切り）。
- **リネームの複数ファイル適用**: `rename` の `WorkspaceEdit` を適用する際、**開いているバッファへの編集は通常編集として undo に載せる**。**未オープンのファイルは開いて編集し保存**するが、**カレントバッファ外の変更は undo 対象に含めない**（複数ファイルの巻き戻しは複雑でバグの温床のため、意図的に対象外）。適用前に対象ファイル数を `notify`／`Confirm` で提示。
- **didChange デバウンス**: 連続入力は約 150ms デバウンスしてから `didChange` を送る（過剰送信と診断のチラつきを抑制）。
- **サーバ異常終了時の再起動**: LSP stdoutのEOFまたはプロトコル読取エラーで、500ms→1s→2sの指数バックオフにより最大3回再起動する。超過したら当該言語のLSP機能を停止し、エディタ自体は継続する。

### 8.4 LspEvent

```rust
enum LspEvent {
    Spawned { server: ServerId },                       // プロセス起動（進捗: 起動中→初期化中）
    Initialized { server: ServerId, caps: ServerCapabilities },
    InitializationFailed { server: ServerId, error: String },
    Progress { server: ServerId, token: ProgressToken, work: WorkDoneProgress }, // $/progress
    Diagnostics { uri: Url, diags: Vec<Diagnostic> },
    Response { id: RequestId, result: Result<Value, ResponseError> },
    Exited { server: ServerId, status: ExitReason },
}
```

initialize応答にerrorがある場合や結果を解釈できない場合は`InitializationFailed`とし、`Initialized`にはしない。クライアントは`window/workDoneProgress/create`、`workspace/configuration`、capability登録などサーバ発の要求へ応答し、サーバを待機させない。

`apply_lsp` はこれらを文書単位の`starting`/`initializing`/`opening`/`coloring`/`checking hover`/`ready`/`updating`/`not found`/`error`へ変換する。`ready`はinitialize・didOpen・semantic tokensに加え、対象Document内の識別子へ送ったHoverプローブが**実際のHover内容（非null）**を返した後だけ表示する。`0:0`へのnull応答は準備完了扱いにしない。冒頭のローカル識別子だけに偏らないよう文書全体を最大12区間に分け、各区間のキーワード以外の識別子へプローブを送り、全サンプルの確認完了までRust Analyzer内部のHover計算を温める。nullなら次候補、errorなら同候補を再試行する。起動直後のクリックは最新1件を保留し、didOpen後に自動送信する。1件も成功しない場合は`ready`を出さない。`WorkDoneProgressBegin/Report`のtitle・message・percentageを`updating: ...`へ整形し、Endで消す。進捗はtoken単位に保持し、別tokenのEndで処理中の進捗を消さない。

### 8.5 補完ポップアップの操作（`Overlay::Completion`）

```rust
struct CompletionState {
    items: Vec<CompletionItem>,   // textDocument/completion の結果
    filtered: Vec<usize>,         // query による絞り込み後の並び
    selected: usize,
    query: String,                // トリガ以降の入力
}
```

- **表示トグル**: `Ctrl+@` / `Ctrl+Space`（`Command::ToggleCompletion`）で開閉。開いていなければ補完要求→表示、開いていれば閉じる。
- **自動表示と絞り込み**: 文字入力後に自動要求し、入力prefixでローカルfuzzy絞り込みする。prefixと完全一致する候補自身は除外する。
- **入力透過**: 候補操作として消費するのは上下・`Enter`・`Ctrl+C`・`Esc`・`Ctrl+@`だけ。他の文字、削除、移動、編集コマンドは通常どおり実行し、候補を再要求する。
- **配置**: 入力中の単語先頭をアンカーにし、同じ単語を入力している間は横位置を固定する。入力カーソルの画面行には重ねず、下に収まれば次行、収まらなければ上側へ出し、選択候補は高コントラスト背景＋太字にする。
- **選択**: 矢印キー ↑/↓。**確定**: `Enter`（選択中の候補を挿入）。**閉じる**: `Esc`。
- 入力を続けると `query` で絞り込み（`fuzzy-matcher`）。Function/Method候補は未指定なら`()`まで挿入し、カーソルを括弧内へ置く。既に括弧を含む候補、および`use`/`import`/`from`文の候補には追加しない。補完要求の失敗は通常編集・保存を妨げないためステータスへ表示しない。
- マルチカーソル時は **primary カーソルのみ**に適用（MVP、実装量を絞る）。

## 9. シンタックスハイライト

### 9.1 2 系統の自動選択

ドキュメントの言語に対し着色元を自動選択する（`Highlighter` を document ごとに保持）。

```rust
enum Highlighter {
    None,
    TreeSitter(TreeSitterState),   // 文法バンドル済み言語
    Lsp(SemanticTokensState),      // LSP稼働言語
}
```

選択規則:

1. その言語に LSP が設定され稼働している → **LSP semantic tokens**。
2. そうでなく、tree-sitter 文法がバンドルされている → **tree-sitter**。
3. どちらも無ければ **None**（無着色）。

→ Rust/Python のような LSP 言語は semantic tokens、TOML/Markdown/JSON のような非 LSP 言語は tree-sitter。言語判定（§11.2）が両者の選択根拠になる。

編集直後も影響範囲外のsemantic tokensを保持し、編集位置より後ろのspanを文字数差分だけ移動する。重なったspanだけを捨て、増分更新済みtree-sitter色へfallbackする。`didChange`後にsemantic tokensを再要求し、request時のDocument versionと現在versionが異なる古い応答は適用しない。画面全体を一瞬白くせず、入力中の色と座標を安定させる。

- 例外: `DocumentKind::Large`（§4.2.1）は言語に関わらず**常に無着色**（`Highlighter` を持たず、tree-sitter も semantic tokens も走らせない）。フリーズ回避のため。

### 9.2 tree-sitter

- バンドル文法（コンパイル時固定）: **TOML / Markdown / JSON**（LSP を前提としない設定・ドキュメント系）。
- 増分パース（編集の `Change` を tree-sitter の edit に変換）→ ハイライトクエリでキャプチャ → トークン種別へ。

### 9.3 テーマ（iceberg dark）

- 内蔵ダークテーマ 1 つ（config 定義なし）。`render/theme.rs`。**iceberg (dark) パレット**に合わせた落ち着いたトーン。
- 参照実装 `my_shell_using_crates/src/editor.rs` の配色をアンカーにする（truecolor `Color::Rgb`）。同ファイルで確定している 4 色はそのまま採用し、エディタ用に不足する背景・前景・選択・行番号等を iceberg dark 標準値で補完する。

```rust
// render/theme.rs  — iceberg dark パレット
// アンカー（参照ファイルと同値）
const BLUE:   Rgb = 0x84a0c6;  // user@host 等 → keyword/function 系
const CYAN:   Rgb = 0x89b8c2;  // cwd 等       → type/string 系
const PURPLE: Rgb = 0xa093c7;  // branch 等    → constant/number 系
const MUTED:  Rgb = 0x6b7089;  // ghost        → comment/行番号 系
// iceberg dark 標準値で補完
const BG:        Rgb = 0x161821;  // 背景
const FG:        Rgb = 0xc6c8d1;  // 通常テキスト
const CURSORLINE:Rgb = 0x1e2132;  // カーソル行
const SELECTION: Rgb = 0x272c42;  // 選択背景
const LINENR:    Rgb = 0x444b71;  // 行番号（非アクティブ）
const GREEN:     Rgb = 0xb4be82;  // string 系の別トーン
const ORANGE:    Rgb = 0xe2a478;  // number/定数
const RED:       Rgb = 0xe27878;  // error/削除（置換プレビューの旧）
const STATUSBG:  Rgb = 0x0f1117;  // ステータスバー背景
// 領域を背景色で分けるための面（§9.3 の枠線なし原則）
const PANE_INACTIVE: Rgb = 0x12141c;  // 非アクティブペインの背景（やや沈める）
const POPUP_BG:      Rgb = 0x1e2132;  // オーバーレイ/ポップアップの浮き面
```

- **枠線を使わない（重要な描画原則）**: box-drawing の罫線でペインやポップアップを囲わない。**領域は背景色の差**で表現する（囲み線に上下左右 2 マスを費やす無駄を省き、狭い SSH 端末で表示領域を最大化する）。
  - **ペイン分割（§4.4）**: 中央の1セルを境界列として`│`を描く。2桁以上の枠は作らない。
  - **端末への反映**: ratatuiのCPU側フレーム構築を完了してから同期更新（DEC mode 2026）を開始し、差分出力直前からだけ画面更新を保留する。改行で行数が増えるフレームは下の行から差分を更新し、端末カーソルは移動後に表示する。同期更新を描画途中で反映するGNOME Terminalでも、行シフト途中や一時カーソルが二重改行・タブに見えない順序を維持する。
  - **オーバーレイ/ポップアップ（§4.6, 補完 §8.5, ホバー）**: 枠で囲わず `POPUP_BG` の塗りブロックとして浮かせる（影の代わりに背景差でレイヤを示す）。
  - **通知（§4.10）**: 独立popupを出さず、ステータスバーへ集約する。
  - **ステータスバー（§9.5）**: `STATUSBG` の 1 行帯。

- **トークン種別 → 色テーブル**: LSP semantic tokens 種別（`keyword`/`function`/`type`/`string`/`comment`/…）と tree-sitter キャプチャ名（`@keyword`/`@function`/`@type`/`@string`/`@comment`/…）の**双方**を、この共通パレットへマップする（`highlight/` からの出力を一元的に着色）。
- **UI 色**: 選択=`SELECTION`、カーソル行=`CURSORLINE`、行番号=`LINENR`/`MUTED`、診断下線=`RED`/`ORANGE`、ステータスバー=`STATUSBG`＋`FG`。置換プレビュー（§13）の旧=`RED`/新=`GREEN`。
- **入力カーソル形状**: primaryは端末の`SteadyBar`（縦棒）、マルチカーソルのsecondaryは描画セル`▏`を使う。正常終了・panic時とも`DefaultUserShape`へ戻す。
- 端末が truecolor 非対応の場合に備え、将来 256/16 色フォールバックを足す余地は残す（v1 は truecolor 前提）。

### 9.4 ガター（行番号列: 診断マーカー ＋ git 差分）

行番号の隣に細い「ガター」列を設け、LSP の error/warning と git の変更状況を可視化する（VSCode の gutter 相当）。レイアウトは左→右で `[git][diag] 行番号 │ 本文`。

- **diag 列（1 桁）**: その行の LSP 診断をseverity順（error→warning→information→hint）に並べ、最大severityを色付きグリフで表示。error=`RED` `×`、warning=`ORANGE` `▵`、information=`CYAN` `i`、hint=`BLUE` `·`、無ければ空白。
- **本文上の診断**: LazyVimの既定を基準に、診断範囲へseverity色の下線を引き、行末には4space＋`●`をprefixにしたseverity色のitalic仮想テキストを置く。コードと仮想テキストは編集上別物で、hit-testにも含めない。
- **git 列（1 桁）**: HEAD との差分で追加=`GREEN`、変更=`BLUE`の縦棒`▌`を表示する。削除はガターに表示しない。変更行のみ保持。

```rust
enum GitLineStatus { Added, Modified, Deleted }   // Deleted = その行の直後に削除がある印
struct GitGutter { lines: HashMap<usize, GitLineStatus> }   // 変更行のみ
```

- **git データ源**: `git` サブプロセスに委譲し、`git diff -U0 --no-color -- <path>` のハンク見出し（`@@ -a,b +c,d @@`）を解析して行範囲を `GitLineStatus` 化する。追加依存なし・SSH 開発サーバでは git 前提で、既存のプロセス起動基盤（LSP/PTY）と一貫。純 Rust にしたい場合は `gix` へ差し替え可能（要相談）。
- **フロー**: `Effect::ComputeGitStatus { doc, path }` → git アクターが`git diff`、`git branch --show-current`、対象ファイルの`git status --porcelain`を実行 → `AppEvent::Git(GitEvent { doc, result: GitInfo })`。`GitInfo`はガター行、branch、対象ファイルのXY状態（`M`/`MM`/`??`等）を持つ。再計算は**ファイルオープン時・保存時**。git管理外のファイルはgit情報を空にする。
- **大容量ファイル（§4.2.1）**: ガターの git/診断は付けない（読み取り専用・LSP なし）。行番号のみ。

### 9.5 画面外インジケータ（エッジ矢印＋件数）

TUI ではミニマップや overview ruler を作らない。代わりに「ビューポート外に何があるか」を**上下エッジの矢印＋件数**と**ステータスバー要約**で示す。対象は 診断（error/warning）・git 変更・検索ヒットの 3 種。

- **エッジ手掛かり**: ビューポートの上端より上／下端より下に対象があれば、上端行の右側に `↑ …`、下端行の右側に `↓ …` のバッジを出す。0 件の種別は出さない。色は §9.3 に従う（error=`RED`、warning=`ORANGE`、git=`BLUE`、検索=アクセント）。
  - 例: 下に error2・warning1・git変更3・追加4 → `↓ E2 W1 M3 A4`。errorは赤、warningはオレンジ、modifiedは青、addedは緑。
- **ステータスバー要約**: 色付きセグメントは「ワークスペース相対path＋未保存/外部変更」「言語とLSP状態（例 `<lsp> rust: updating`。非LSPは`<syntax> markdown`）」「Git状態（例 `<git> clean @main`）」「診断総数 `E:5 W:2`」に絞る。LSP状態は`starting`/`initializing`/`opening`/`coloring`/`checking hover`/`ready`/`updating`/`not found`/`error`と進捗本文を区別する。行数・列数・割合は表示しない。
- **長い行**: ペイン幅でsoft wrapするため、横方向の`»`/`«`は表示しない。
- **算出**: `View.scroll` と表示高から、上/下それぞれの件数を集計する**純粋関数** `offscreen_cues(diagnostics, git_gutter, search_hits, scroll, height) -> OffscreenCues` にまとめてテストする。追加の `AppEvent`/`Effect` は不要（描画時に既存状態から計算）。

```rust
struct OffscreenCues { above: EdgeSummary, below: EdgeSummary }
struct EdgeSummary { errors: usize, warnings: usize, git_changes: usize, search_hits: usize }
```

- 検索ヒットのエッジ/要約は、検索が有効な間（`Overlay::Search` 表示中）に出す。診断・git は常時。大容量ファイルは読み取り専用状態だけを表示する。

### 9.6 診断メッセージの表示（ホバー＋ステータスバー）

診断の**場所**（ガター §9.4／件数・画面外バッジ §9.5）に加え、**メッセージ本文**（例 `unused variable \`a\``）は行末virtual text・ステータス・ホバーに出す。専用の診断一覧オーバーレイは持たない。

- **ステータスバー**: カーソル行に診断があれば、最優先（error > warning）1 件のメッセージを 1 行で常時表示。ソースも添える（例 `⚠ unused variable \`a\` — rustc(unused_variables)`）。at-a-glance 用。
- **行末virtual text**: 各論理行で最優先の診断を`› message`としてseverity色＋italicで描画する。Rope/Selectionには入れず、クリックやカーソル移動の対象にしない。
- **ホバーtooltip（`Overlay::Hover`, §4.6）**: 通常クリックで入力カーソルを単一caretへ移した時に、**LSP hover情報＋その位置の全診断**を集約表示する（focusを奪わない）。左右エディタ分割中は操作中ペインと反対側（右操作中は左）へ表示し、中央の分割線側へ寄せて操作位置との距離を抑える。Markdownをtree-sitterで、Rustのコードフェンス・シグネチャ・例をRust文法で再着色する。MouseMovedや範囲選択中は要求せず、選択開始時は既存Hoverを閉じる。Hover単体の要求失敗は通常statusへ出さず、表示を閉じる。
- **初回コストの前倒し**: tree-sitterのMarkdown/Rust highlight queryは`OnceLock`で共有し、設定読込時に短いサンプルをparseして初回コンパイルを済ませる。LSP側も上記の識別子プローブで実Hover計算を`ready`前に実行する。
- 波線下線の色は severity に対応（error=`RED` / warning=`ORANGE`, §9.3）。`Editable.diagnostics` が唯一の source（gutter・下線・ステータス・ホバーはすべてここから描画時に導出）。

## 10. クリップボード

- **内部レジスタ** `Register` を常に保持（マルチカーソル対応のため行/複数片も保持可能）。
- yank/copy 時、内部レジスタ更新に加えて `Effect::ClipboardOsc52(text)` を発行し、OSC52 エスケープで **手元端末（SSH クライアント側）のシステムクリップボード**へ転送する。
- 選択が全て空の`Ctrl+C`は、各カーソルの論理行を改行付きのlinewiseレジスタとしてコピーする。linewise pasteはカーソル行の行頭へ挿入し、行の途中へ内容を割り込ませない。選択があれば従来どおり選択範囲を文字単位でコピーする。
- paste は内部レジスタから（端末→アプリ方向の OSC52 read は端末対応が不安定なため使わない）。ブラケットペーストは端末の貼り付けとして受け、複数行を 1 編集として扱う。

## 11. 設定

### 11.1 ファイル

- 位置: `~/.my_editor_rc.toml`。
- 読み込みは起動時の `Effect::LoadConfig` → `AppEvent::ConfigLoaded`。パース失敗は致命的にせず既定値へフォールバックし、ステータスに警告。

### 11.2 スキーマ（言語判定と LSP コマンド）

言語判定ルールと、言語ごとの LSP コマンド名はユーザ設定可能。tree-sitter の適用対象もこの言語判定に従う（判定された言語名に対応する文法がバンドルされていれば適用）。

```toml
[[language]]
name = "rust"
extensions = ["rs"]
lsp = ["rust-analyzer"]          # コマンド＋引数。省略で LSP なし

[[language]]
name = "toml"
extensions = ["toml"]
line_comment = "#"               # コメントトグル用（§7.4）
# lsp 無し → tree-sitter（toml文法）で着色

[[language]]
name = "make"
filenames = ["Makefile", "makefile", "GNUmakefile"]
tab_size = 4
insert_spaces = false             # Make recipeでは実タブを入力

[editor]
tab_size = 4
insert_spaces = true
shell = "/path/to/shell"          # 省略時は $SHELL、さらに無ければ /bin/sh
large_file_threshold = "10MiB"   # これを超えると less 相当の読み取り専用で開く（§4.2.1）

[search]                          # Ctrl+F / Ctrl+H の Directory スコープ既定（§13）
respect_ignore_files = true       # .gitignore / .ignore を尊重
include_hidden = false
exclude = ["**/.git/**", "**/target/**", "**/node_modules/**"]
```

```rust
struct Config {
    languages: Vec<LanguageConfig>,
    editor: EditorConfig,
    search: SearchConfig,          // SearchFilters の既定（§13）
}
struct LanguageConfig {
    name: LanguageId,
    extensions: Vec<String>,
    filenames: Vec<String>,        // Makefile等の拡張子なしファイルを完全一致判定
    lsp: Option<LspCommand>,       // Vec<String>: argv
    line_comment: Option<String>,  // コメントトグル記号。無ければバンドル既定値
    tab_size: Option<usize>,       // 未指定ならEditorConfigを継承
    insert_spaces: Option<bool>,   // 未指定ならEditorConfigを継承
}
struct EditorConfig {
    tab_size: usize,
    insert_spaces: bool,
    shell: Option<String>,
    large_file_threshold: u64,     // バイト。既定 10 MiB（§4.2.1）
}
struct SearchConfig {              // SearchFilters（§13）の既定シード
    respect_ignore_files: bool,
    include_hidden: bool,
    exclude: Vec<String>,
}
```

- 判定: `filenames`の完全一致を確認し、次に拡張子から`LanguageId`を決める。将来shebang規則を足す余地を残す。
- `line_comment` は `Ctrl+/` / `Ctrl+Q` のコメントトグルに使う。未指定ならバンドル言語の既定値。

### 11.3 workspace_root の決定

`Editor.workspace_root`（finder / ディレクトリ検索 / LSP の root, ピッカーの `inside_root` 判定に使う）は起動時に次で決める:

- 基準は**起動時の cwd**。
- cwd から上位へ辿り、**最初に見つかった `.git` を持つディレクトリを root** とする。
- どこにも `.git` が無ければ **cwd をそのまま root** とする。

## 12. 統合ターミナル

- 分割モード 2 の右ペインに **単一シェルセッション**。
- PTY は `nix`（openpty）で確保。マスタ fd を tokio の `AsyncFd` でラップして非同期読み取り、`AppEvent::Terminal(Output(bytes))` を発行するアクタータスク。
- 画面状態は `vt100::Parser` を `TerminalSession` に保持し、`Output` 受信時にパーサへ供給。描画はパーサの screen を ratatui セルへ変換。
- 入力は shell focus 時にバイト列化して `Effect::TerminalInput`。リサイズは `TIOCSWINSZ`（nix）＋ `parser.set_size`。

```rust
struct TerminalPane { session: TerminalSession }
struct TerminalSession { parser: vt100::Parser, size: (u16, u16) }
enum TerminalEvent { Output(Vec<u8>), Exited(ExitReason) }
```

- **`Ctrl+O`**: シェル未起動なら起動してペインを表示する。起動済みならプロセスを終了せず、ペインの表示/非表示だけを切り替える。
- **シェル文字選択**: シェルペインを左ドラッグすると、選択開始時のvt100画面をスナップショットとして固定し、選択範囲を反転表示する。ドラッグ完了時に選択文字列をOSC52で端末エミュレータのクリップボードへ送る。選択中もPTY出力は内部parserへ取り込むが表示とコピー元はスナップショットのままにし、後続出力で内容を変えない。キー入力時に選択を解除して最新画面へ戻る。選択中の`Ctrl+C`はコピー、選択がなければ従来どおりSIGINTを送る。
- **スクロールバック**: vt100 parserへ10,000行を保持し、シェルペイン上のホイールで3行ずつ移動する。キー入力・貼り付け時は最新行へ戻る。
- **Hoverとの排他**: `Ctrl+O`でシェルを表示した時点、およびシェルペインをクリックした時に、エディタ側Hoverと保留・処理中のHover要求を消す。terminal focus中はHoverを表示しない。
- **シェル終了時**: 子シェルが`exit`または`Ctrl+D`で終了し、PTYから`TerminalEvent::Exited`を受けた時だけセッションを破棄してシェルペインを閉じる。終了した旨の通知は出さない。再度`Ctrl+O`で新しいシェルを起こせる。

## 13. 検索・走査

- **ピッカー（`Ctrl+T` / `Ctrl+G` / `F6` / `Ctrl+P`）**: `Overlay::Picker` に統合し、候補ソースだけが異なる。すべて`fuzzy-matcher`でランキングする。表示は選択位置周辺の最大5件に仮想化し、前後に候補があれば`…`と総件数を表示する。一致文字は黄色＋boldにする。入力prefixごとのランキングを小さなcacheへ保持し、Backspace時は直前prefixの結果を再計算せず復元する。空queryへのファイル走査batch追加は全件再sortせずindexだけを追記する。Pickerを開く時は既存Hoverを消し、開いている検索・置換ペインを閉じて入力先をPickerへ一本化する。`Ctrl+T`/`Ctrl+G`は同じPickerの再押下で閉じ、Picker矩形外の左クリックでも閉じる。矩形内のクリックは候補操作には使わない。ファイルソースは**入力をパスとして解釈する直接オープンのフォールバック**も持つ。

  ```rust
  enum PickerSource {
      Directory,     // Ctrl+T: workspace_root 以下（ignore walk, 非同期）
      OpenBuffers,   // Ctrl+G: 開いている全 Document（同期）
      DiffTarget { base: DocumentId }, // F6: baseと同じDocument/同じpathを除く（同期）
      Commands,      // Ctrl+P: コマンド名・説明・キーバインド（同期）
  }

  struct PickerState {
      source: PickerSource,
      query: String,
      candidates: Vec<PickerItem>,        // source 由来（Directory=相対パス, OpenBuffers=doc名/パス）
      ranked: Vec<usize>,                 // fuzzy ランキング結果
      direct: Option<DirectPathOpen>,     // query をパス解釈した直接オープン候補
      selected: usize,
  }

  struct DirectPathOpen {
      input: String,     // 入力そのまま（例 "../a/b.rs", "/etc/hosts", "~/x"）
      resolved: PathBuf, // ~ 展開・workspace_root 基準で正規化
      exists: bool,      // 非存在時の扱いは下記規則（root配下＋親存在なら新規バッファ）
      inside_root: bool, // resolved が workspace_root 配下か（カレント以下か）
  }
  ```

- **バッファ比較（`F6`）**: `PickerSource::DiffTarget` で現在のDocumentおよび同じファイルpathを持つDocumentを除外して比較対象を選ぶ。確定後は `Layout::EditorAndEditor` に切り替え、現在のバッファを左、比較対象を右に固定する。行単位 diff で対応する行を上下に揃え、追加=`GREEN`、削除=`RED`、変更=`ORANGE` で表示する。片側に対応行がない箇所は空の diff 行を挿入して位置を揃える。diff 表示中も各ペインの原文は通常どおり編集可能で、編集後は差分を再計算する。
- **コマンドパレット（`Ctrl+P`）**: `PickerSource::Commands`で、実行可能なコマンドを「キーバインド / コマンド名 / 短い説明」の1行候補として表示する。名前・説明・キーのすべてをfuzzy検索対象にし、`↑`/`↓`で選択、`Enter`でPickerを閉じて実行する。固定キーがないコマンドは`—`と表示する。操作時にキーバインドを反復表示し、利用しながら覚えられることを目的とする。

  - **Directory ソース**: `ignore`（ripgrep 由来・高信頼）の `WalkBuilder` で `.gitignore` を尊重しつつ列挙。結果を `AppEvent::FileScan` でバッチ供給し、`ScanToken` で古い走査を破棄。ランキングはクエリ変更で再計算。
  - **OpenBuffers ソース**: `Editor.documents` を候補にする同期リスト（走査不要）。
  - **直接オープンのフォールバック（両ソース共通）**: 入力が `/`（絶対）・`~`・`./`・`../` を含むなど**パスらしい**場合、`direct` を作る。候補に fuzzy マッチしなくても `direct` エントリを常に選べるようにし、Enter で開ける。存在時・非存在時で振る舞いを分ける:
    - **存在する** → 既存ファイルを開く（`Effect::ReadFile`。大容量なら §4.2.1）。ワークスペース外（絶対 / `../` で外へ抜ける）でも、存在すれば開く。
    - **存在しない**:
      - `inside_root`（カレントディレクトリ以下）**かつ親ディレクトリが存在** → 空の新規バッファをそのパスに紐付けて開く。`Ctrl+S` で初めてファイル作成（`Effect::WriteFile`）。
      - `inside_root` だが**親ディレクトリが存在しない** → **エラー**（`notify(Error, "ディレクトリが存在しません")`、開かない）。
      - **ワークスペース外**（絶対 / `../` で外）→ 開かない（存在しないパスの新規作成はしない。`notify(Info)` 程度）。
    - 補足: 無名（path=None）バッファ（起動時の空バッファ等）は保存対象外。`Ctrl+S` は無効で `notify` するのみ（無名バッファの保存フローは持たない）。
- **grep エンジン（Directory スコープの実体）**: `grep`（`grep-searcher` + `grep-regex`, BurntSushi 製・高信頼）で内容検索。別タスクで実行し、ヒットを `GrepToken` 付きでバッチ送出。検索オーバーレイの `Directory` スコープ（§13 の `SearchState`）から `Effect::StartGrep` 経由で使う。
- **検索・置換**（`Ctrl+F` / `Ctrl+H`）: 単一のオーバーレイ `Overlay::Search` に統合し、**スコープを切り替えられる**。3 つの検索オプションはどのスコープでも共通。

```rust
enum SearchScope {
    CurrentBuffer,   // カレントバッファのみ（同期）
    AllBuffers,      // 開いている全 Document を横断（同期）
    Directory,       // workspace_root 以下をディスク走査（grep, 非同期）
}

struct SearchOptions {
    case_sensitive: bool,   // aA  : 大文字小文字を区別
    whole_word: bool,       // word: 単語単位（境界一致）
    regex: bool,            // .*  : 正規表現（false のときはリテラル検索）
}

struct SearchState {          // Overlay::Search（§4.6）
    query: String,
    replace: Option<String>,  // Ctrl+H のとき Some
    scope: SearchScope,
    options: SearchOptions,
    filters: SearchFilters,   // ファイル条件・ignore 設定（Directory スコープで有効）
    results: SearchResults,    // スコープにより中身が変わる
    current: usize,
}

struct SearchFilters {
    include: Vec<String>,        // 対象に含める glob（例 "src/**/*.rs"）。空なら全部
    exclude: Vec<String>,        // 除外する glob（例 "**/target/**"）
    respect_ignore_files: bool,  // .gitignore / .ignore を尊重（既定 true）
    include_hidden: bool,        // 隠しファイルも対象（既定 false）
}

enum SearchResults {
    InBuffer { doc: DocumentId, ranges: Vec<Range<CharIdx>> },     // CurrentBuffer
    CrossBuffer { hits: Vec<BufferHit> },                          // AllBuffers（doc + range）
    OnDisk { token: GrepToken, hits: Vec<GrepHit> },               // Directory（path + line, 非同期）
}
```

- **スコープ循環**: `Ctrl+F`（置換なら `Ctrl+H`）を、オーバーレイが**閉じている**ときは `OpenSearch` として開く。**開いている**ときは `CycleSearchScope` として `CurrentBuffer → AllBuffers → Directory → CurrentBuffer …` と回す。翻訳層（§7.1）が `overlay: Some(Search)` か否かで両者を出し分ける。`Ctrl+Shift+F` は最初から `Directory` スコープで開く（`OpenSearchInDirectory`）。
- **スコープごとの解決**:
  - `CurrentBuffer` / `AllBuffers`: 同期。`update` 内でヒットを計算（`regex` クレート）。結果は `CharIdx` 範囲。
  - `Directory`: 非同期。`Effect::StartGrep` を発行し、`grep`（`grep-searcher`/`grep-regex`）で走査、`AppEvent::Grep` でヒットをストリーム受信。`GrepToken` で古いクエリを破棄。
- **ファイル条件・ignore 設定（`SearchFilters`。Directory スコープで有効）**: 検索オーバーレイに include/exclude の glob 入力欄と ignore トグルを設ける（VSCode の files-to-include / exclude 相当）。`ignore` クレートへ次のようにマップする:
  - `include` / `exclude` → `ignore::overrides::OverrideBuilder`（`exclude` は `!pattern` として追加）。
  - `respect_ignore_files` → `WalkBuilder::git_ignore(_)` / `ignore(_)`（`.gitignore` / `.ignore` の尊重可否）。
  - `include_hidden` → `WalkBuilder::hidden(!include_hidden)`。
  - 既定値は config から与える（下記）。`GrepQuery`（`Effect::StartGrep`）に `filters` を載せて走査タスクに渡す。
- **検索式の構築**: `regex=false` でも内部的に `regex` クレートへ流す（`regex::escape` でリテラル化、`whole_word` は `\b…\b`、`case_sensitive=false` は `(?i)` フラグ）。3 オプションを 1 つの `Regex` 構築に集約する純粋関数にして、`Directory` の `grep-regex` 設定にも同じロジックをマップする（挙動統一・テスト対象）。
- **置換プレビュー（`Ctrl+H`）**: 置換は**確定するまでバッファ／ファイルを一切変更しない**。クエリ・置換文字列・オプションから各マッチの before→after を算出し、分かりやすく提示してから適用する。

  ```rust
  struct ReplacePreview {
      items: Vec<ReplaceItem>,   // 全マッチぶん
      applied: bool,             // まだ未適用
  }
  struct ReplaceItem {
      location: MatchLocation,   // doc/path + 行番号
      line_before: String,       // 該当行（マッチ強調）
      line_after: String,        // 置換後の同じ行（regexキャプチャ $1.. 展開済み）
      selected: bool,            // 個別に対象から外せる
  }
  ```

  - **表示**: マッチ行を before/after で並べる（削除色=旧、追加色=新）。`AllBuffers`/`Directory` はファイル単位でグルーピング。件数と対象ファイル数をヘッダに出す。
  - **適用単位**: 「現在のマッチのみ」「全て」を選べ、`selected=false` の項目は除外。適用は 1 つの undo トランザクションにまとめる（§4.5）。
  - **regex 展開**: `after` はキャプチャ参照（`$1`, `${name}`）を反映した実際の結果を見せる（文字列テンプレート適用は純粋関数にしてテスト）。
  - **スコープ別の確定**: `CurrentBuffer`/`AllBuffers` は編集コマンド化して undo に載る。`Directory` は開いていないファイルの書き換えを伴うため、プレビュー確定時に加えて `Overlay::Confirm` で最終確認を挟む（ディスク書き込みは元に戻しにくいことを明示）。
- オプションのトグルキー（オーバーレイ focus 時）は実装時に確定（例: `Alt+C`=aA / `Alt+W`=word / `Alt+R`=regex）。無効な正規表現はエラー表示にとどめ落とさない。

```rust
enum GrepEvent  { Hits { token: GrepToken, hits: Vec<GrepHit> }, Done(GrepToken) }
enum FileScanEvent { Batch { token: ScanToken, paths: Vec<PathBuf> }, Done(ScanToken) }
```

## 14. エラーハンドリング

- サブシステムごとに `Error` 型を独立定義（`LspError` / `TerminalError` / `IoError` / `ConfigError` / `SearchError`）。トップレベル `Error` へ `From` で接続（原則 4）。
- Effect 実行の失敗は panic させず `AppEvent::Error(String)`（またはより具体的なイベント）としてループに戻し、ステータス行／確認オーバーレイで通知。
- 起動・端末 raw mode 取得など初期化の失敗のみ致命的として終了。
- **パニック時の端末復帰（必須）**: raw mode ＋代替スクリーン中にパニックすると SSH セッションが壊れたまま残る。これを防ぐため:
- 端末セットアップを **RAII ガード**（`Drop` で raw mode 解除・alt screen 退出・カーソル表示・カーソル形状の既定値復元・マウス無効化）にして、正常終了でもパニックでも必ず復元する。
  - 起動時に **panic hook** を差し込み、端末を復元してから元の hook を呼びバックトレースを出す（画面破壊を防ぎつつ情報は残す）。
  - 統合ターミナルの PTY 子プロセスも終了時に確実に kill/クローズする。

### 14.1 保存の安全性とファイル外部変更の監視

- **アトミック保存**: 同ディレクトリの一時ファイルへ書いて fsync → `rename` で置換（書き込み途中のクラッシュや満杯でも元ファイルを壊さない）。既存ファイルのパーミッションを可能な範囲で維持。
- **保存時の衝突検出**: 保存直前に、直近同期時の mtime/size と現在ディスクを比較。食い違えば `Confirm`（上書き / 中止）を挟む。
- **外部変更の監視**: ランタイムが開いている各バッファのパスの mtime/size を**定期的に stat**（数秒間隔のタイマ、`HashMap<DocumentId, DiskState>` に前回値保持）。変化検出で `AppEvent::Io(ExternalChange { doc })`。
  - 未変更バッファ → **自動再読込 ＋ `notify(Info)`**。
  - 変更ありバッファ → **`notify(Warn)` で競合を通知**（自動上書きはしない）。
  - 自分の保存直後は `DiskState` を更新して自己誤検出を防ぐ。
- **ディレクトリ置換との整合（§13）**: Directory スコープ置換がディスクに書いたファイルが開かれていても、この監視が変化を捉えて開いているバッファへ反映（未変更なら再読込）。専用配線は不要でこの仕組みに集約。
- 大容量ファイル（§4.2.1, read-only）は監視対象だが再読込は mmap 再マップで対応。

## 15. モジュール構成

```
src/
  main.rs                 # 引数(std::env::args)で開くファイルパスを受け取る、raw mode、Runtime起動
  runtime/
    mod.rs                # Runtime, event loop, effect実行, drain/coalesce
    input.rs              # crossterm購読 → RawInput
  editor/
    mod.rs                # Editor, update() ディスパッチ, dirty管理
    command.rs            # Command enum
    event.rs              # AppEvent enum（+ From）
    effect.rs             # Effect enum
    focus.rs              # Focus, Side
    layout.rs             # Layout（3モード）, EditorPane
  document/
    mod.rs                # Document, DocumentKind, LineEnding
    editable.rs           # Editable（Rope・編集ロジック）
    large_file.rs         # LargeFile, LineIndex（mmap ページング, 読み取り専用。§4.2.1）
    history.rs            # History, Revision, Change（undo/redo）
    hash.rs               # 保存時点との内容比較に使う安定ハッシュ
    edit.rs               # Change適用, ropeラッパ
  view/
    mod.rs                # View, Scroll
    selection.rs          # Selection, Selections（不変条件・マージ）
    movement.rs           # カーソル移動（文字/単語/行/文書）
  position/
    mod.rs                # CharIdx, DisplayPos, LSP/表示幅 変換
  input/
    keymap.rs             # 内蔵keymap, KeyChordState
    translate.rs          # (RawInput, focus) → AppEvent
  render/
    mod.rs                # draw(&Editor)
    editor_view.rs        # ガター(git+診断)・行番号・テキスト・カーソル・選択・診断下線（§9.4）
    terminal_view.rs      # vt100 screen → cells
    overlay.rs            # 各オーバーレイの描画
    notifications.rs      # 右下トースト＋進捗（§4.10-4.11）
    statusline.rs         # ファイル・Git branch/状態・診断総数（§9.5）
    offscreen.rs          # エッジ矢印＋件数（offscreen_cues, §9.5）
    theme.rs              # 内蔵テーマ, token→color
  overlay/
    mod.rs                # Overlay enum（+ From）
    picker.rs             # Ctrl+T/Ctrl+G ピッカー＋パス直接オープン（§13）
    search.rs             # 検索/置換・スコープ循環・置換プレビュー（§13）
    completion.rs hover.rs rename.rs confirm.rs   # hover は診断本文も集約（§9.6）
  lsp/
    mod.rs                # LspRegistry, ServerId, pending管理
    actor.rs              # 子プロセス＋read/write スレッド（lsp-server）
    convert.rs            # lsp-types ↔ 内部型（位置変換含む）
    semantic.rs           # semantic tokens ハイライト
    event.rs              # LspEvent, LspOutbound
  terminal/
    mod.rs                # TerminalSession, TerminalEvent
    pty.rs                # nix PTY, AsyncFd 読み取り
  search/
    scan.rs               # ignore walk（finder）
    grep.rs               # grep crate
  git/
    mod.rs                # GitGutter, GitEvent, git diff -U0 のハンク解析（§9.4）
  highlight/
    mod.rs                # Highlighter enum・選択規則
    tree_sitter.rs        # 文法バンドル・増分パース・クエリ着色
  clipboard.rs            # Register, OSC52
  config/
    mod.rs                # 読込・serdeスキーマ
    language.rs           # 言語判定（拡張子→LanguageId）
  error.rs                # 各Error＋トップレベル（From）
```

## 16. 依存クレート（Cargo.toml 変更方針）

既存に加える／変える:

- 追加: `tree-sitter` と文法（`tree-sitter-toml` / `tree-sitter-md` / `tree-sitter-json`）、`unicode-width`、`ignore`、`grep`、`memmap2`（大容量ファイルの mmap。§4.2.1）。
- `crossterm` に `event-stream` フィーチャを付与（非同期入力購読）。
- **削除**: `clap`（複雑な起動オプションを設けないため。引数は `std::env::args` で任意のファイルパスを受けるだけ）。`walkdir` も `ignore` に置換できるため削除候補（実装時に判断）。
- 既存の `lsp-server` / `lsp-types` / `ropey` / `ratatui` / `nix` / `vt100` / `tokio` / `fuzzy-matcher` / `regex` / `serde` / `toml` はそのまま活用。

信頼度: tree-sitter（GitHub）、ignore/grep（ripgrep・BurntSushi）、lsp-server（rust-analyzer）、unicode-width は広く使われる高信頼クレートのみを採用する。

## 17. テスト方針

「検証価値のある振る舞い」にのみテストを書き、実装の写経はしない。`update` が純粋同期関数であることを最大限活かす。

- **update 単体**: `AppEvent` 列 → 期待する state と `Vec<Effect>` を assert（編集・移動・オーバーレイ遷移）。
- **編集・undo**: `Change` 適用の可逆性、undo/redo のグルーピング境界、マルチカーソル編集のオフセット補正。
- **位置変換**: `CharIdx ↔ DisplayPos ↔ LSP Position` の境界（CJK 全角・タブ展開を含む。結合文字は範囲外と明記）。
- **keymap 翻訳**: 代表的な (focus, key) → AppEvent。
- **選択**: 重なりマージ・collapse・非空不変条件。
- **fuzzy ランキング**: 期待順序の安定性。
- **tree-sitter / vt100**: 代表的な入力 → トークン／screen の重要ケース（結合テスト最小限）。

テストしないもの: 実際の描画ピクセル、実 LSP/シェルプロセスの起動（ごく一部の smoke のみ）。

## 18. 実装フェーズ

上から順に、各フェーズ単体で動く状態を保って進める。

- **P0 ランタイム骨格**: event loop、`AppEvent`/`Effect`/`update` の空実装、描画スケルトン、終了処理、raw mode。
- **P1 エディタコア**: Document/Rope、単一カーソルの移動・挿入・削除、スクロール、行番号描画、undo。
- **P2 マルチカーソル＋基本マウス**: `Selections`、選択拡張、`Ctrl+D`/カーソル追加、コピー・カット・ペースト（内部＋OSC52）、**基本マウス（クリック/ドラッグ選択/ホイールスクロール/ダブル・トリプルクリック）**。マウスが操作の主役のため早期に入れる。
- **P3 ファイル入出力**: open/save、行末保持、config 読込、言語判定、**メモリ内undo履歴と保存タグ（§4.5.1）**、**大容量ファイルの mmap ページング（§4.2.1）**。
- **P4 オーバーレイ基盤**: overlay 描画・focus、ピッカー（`Ctrl+T`/`Ctrl+G`/`F6`/`Ctrl+P`, ignore走査＋fuzzy＋パス直接オープン＋コマンド検索）、F6の左右diff表示、スタートページ（§4.12）。
- **P5 検索**: バッファ内検索・置換、プロジェクト grep（grep クレート）。
- **P6 tree-sitter ハイライト**: TOML/Markdown/JSON、増分パース。
- **P7 LSP 基盤**: アクター（lsp-server フレーミング）、initialize、ドキュメント同期、診断表示、**ガター（診断＋git 差分列, §9.4）**。
- **P8 LSP 機能**: 補完・ホバー・定義ジャンプ・リネーム・整形、semantic tokens ハイライト。
- **P9 統合ターミナル**: PTY（nix）、vt100、分割モード 2、入力転送・リサイズ。
- **P10 仕上げ**: マウスの高度機能（`Ctrl+`クリック定義ジャンプ）、レイアウト切替の磨き込み、エラー通知 UX。

## 19. 決定ログ / 次タスク

- **採用ショートカットの整理（完了 → §7.4 に確定反映）**: 右手マウス中心・左手キーボード最小の方針で取捨選択。主な確定: スクロールはホイールのみ（`PageUp/Down`不採用）、ファイルピッカーは`Ctrl+T`、コマンドパレットは`Ctrl+P`、左右分割`Ctrl+]`・シェル`Ctrl+O`（フォーカス追従、専用フォーカスキー無し）、定義=Ctrl+クリック・ホバー=自動、コメントトグルは`Ctrl+/`と`Ctrl+Q`の2キー。`Command`（§5.3）と`Overlay`（§4.6）に反映済み。
- 解決済み: 補完ポップアップUX（§8.5, `Ctrl+@`/`Ctrl+Space`トグル、矢印選択、Enter・Tab確定）。ディレクトリ検索結果一覧（＝旧「grep結果」, §13 `SearchResults::OnDisk`）。無名バッファの保存フローは**持たない**（保存対象外, §13）。
- 懸念の決着（前回提示分）:
  - パニック時の端末復帰 → **実装**（§14: RAII ガード＋panic hook）。
  - undo 永続化のプライバシー → **0700/0600**（§4.5.1）。
  - LSP × マルチカーソル → **primary のみ（MVP）**（§8.3, §8.5）。
  - workspace_root → **cwd 基準・上位 `.git` を root**（§11.3）。
  - OSC52 制約 → **放置**（内部レジスタが真実源, §10）。
  - 端末のキーエンコーディング限界 → **いったん放置**（実機で取れない組合せが出たら再検討）。
- 追加決定（本バッチ）:
  - 終了 = **F4**、バッファを閉じる = **Ctrl+W**（§7.4）。
  - バイナリ/非 UTF-8 は開かず警告（§4.2.2）。
  - **アトミック保存**＋保存時衝突検出＋**外部変更の mtime 監視**（§14.1）。未保存インジケータ `●`（§9.5）。
  - stale 診断は行デルタ暫定シフト＋淡色化（§8.3）。
  - リネーム複数ファイル適用：**カレントバッファ外は undo 対象外**（§8.3）。
  - Directory 置換と開いているバッファの整合は外部変更監視に集約（§14.1）。
  - シェル終了でペインを閉じる（§12）。ディレクトリ/無指定起動は**スタートページ**（§4.12）。
  - 編集補助（オートインデント・単語境界・括弧対応, §5.5）。
  - LSP: didChange 150ms デバウンス、異常終了は指数バックオフ再起動（§8.3, 既定は当方推奨）。
  - 診断メッセージ本文の表示先＝**ホバー＋ステータスバー**（§9.6）。**診断一覧オーバーレイは廃止**（§4.6・§15 から削除）。
