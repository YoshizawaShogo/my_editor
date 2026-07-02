# 実装進捗

## 2026-07-01

- `DESIGN.md` 全体とリポジトリ構成を確認した。
- 初期状態は `Cargo.toml`、設計書、キーマップ文書のみで、`src/` は未作成。
- 実装順序は `DESIGN.md` §18 に従い、P0（ランタイム骨格）から着手する。
- P0 の event loop、`AppEvent` / `Effect` / `Editor::update`、入力タスク、描画スケルトン、F4 終了、端末復元を実装した。
- `cargo test`（4件）、`cargo clippy --all-targets -- -D warnings`、PTY 上の F4 終了を確認した。終了時に raw mode・代替画面・マウスキャプチャが解除されることも確認した。
- P1 に着手。ファイル IO は P3 のため、P1 の動作確認には一時的な無名バッファを使う。
- P1 の Rope バッファ、単一選択、文字/単語/行/文書移動、選択拡張、挿入、改行時オートインデント、前後削除、undo/redo、縦横自動スクロール、行番号・選択・カーソル・変更状態の描画を実装した。
- 位置変換は char index を基準とし、タブ幅と CJK 表示幅を考慮する純粋関数として分離した。
- P1 実装後は単体テスト 15 件、`clippy -D warnings`、PTY 上で `abc` 入力→左移動→削除→undo→F4 終了を確認した。
- `cargo deny check` は advisory / bans / licenses / sources の全検査に成功した（推移依存の重複バージョン警告は `issue.md` に記録）。
- 追加仕様として F6 の比較対象バッファピッカーと左右 diff 表示を `DESIGN.md`・P4 の実装対象へ反映した。
- P2 のマルチカーソル編集、重複/重なり選択の正規化、全選択、`Ctrl+D`、`Ctrl+Alt+↑/↓`、選択単一化を実装した。
- 内部レジスタ、コピー/カット/貼り付け、OSC52 出力、ブラケットペーストを実装した。
- クリック、Alt+クリック、ドラッグ、ホイール、ダブルクリック単語選択、トリプルクリック行選択を実装した。連続クリック判定は入力層で500ms以内・同一座標を数え、`update` の純粋性を維持した。
- P2 完了時点で単体テスト 26 件と `clippy -D warnings` が成功した。
- P3 の入口として複数のコマンドラインファイルを actor 側で読み、UTF-8/バイナリ判定後に `IoEvent` でバッファへ反映する処理を実装した。CRLF は内部 LF に正規化し、元の行末種別を保持する。
- `Ctrl+G` の開いているバッファピッカーと F6 の比較対象ピッカーを実装した。候補は fuzzy 絞り込みできる。
- F6 確定後は現在バッファを左、比較対象を右に配置し、LCS ベースの行 diff で対応行を揃える。追加・削除・変更は別背景色と `+`/`-`/`~` で表示する。
- `cargo run -- DESIGN.md README.md` を PTY で起動し、F6→候補表示→Enter→左右 diff→F4 の一連動作と端末復元を確認した。
- F6 実装後は単体テスト 29 件と `clippy -D warnings` が成功した。
- P3: Ctrl+S、アトミック保存、CRLF復元、0700/0600のundo履歴永続化、FNVキーと内容hash検証、`~/.my_editor.toml`、言語判定、10MiB超のmmap読み取り専用ビューを実装した。
- P4: `Ctrl+T` のignore対応非同期ファイル走査、fuzzyピッカー、スタートページを実装した。
- P5: カレント/全バッファ/ディレクトリの検索、actorによるストリーミングgrep、バッファ置換を実装した。
- P6: tree-sitter 0.25とJSON/TOML/Markdown文法を組み込み、iceberg配色で着色した。
- P7: LSP子プロセスactor、initialize、didOpen/didChange、診断受信、診断・gitガターを実装した。
- P8: completion request/response、候補ポップアップ、選択・確定を実装した。
- P9: openpty、制御端末設定、actor読取、vt100画面、入力転送、左右シェルペイン、終了処理を実装した。PTY実機相当テストで `printf hi` の入出力を確認した。
- P10: 左右分割、マウスでのペインフォーカス、ヒントガイド、未保存終了保護、バッファクローズ、コメント/インデントキー、gitガターを実装した。
- P8/P10: Ctrl+クリックのLSP定義要求、response対応付け、既存/未オープン定義ファイルへの遷移を実装した。
- 現時点でテスト38件、`clippy --all-targets -- -D warnings`、`git diff --check` が成功している。

## 2026-07-02

- undoを750msの連続文字入力単位でグループ化し、移動時にグループを閉じるようにした。
- tree-sitterを描画時の全再パースから、`InputEdit`と旧Treeを使う増分パース・spanキャッシュへ変更した。
- P4の直接パス入力を実装した。既存ファイル、`~/`、相対/絶対パス、ワークスペース内の新規ファイルを扱う。
- P5のcase-sensitive/whole-word/regexを共通Regex構築へ統合し、Directory検索にも同じ式を渡すようにした。include/exclude glob、ignore、hidden設定をオーバーレイと走査へ接続した。
- Directory置換を、対象ファイル数のConfirm後にactorでアトミック適用するようにした。regexキャプチャ展開にも対応した。
- P8のhover（マウス位置＋同じ行の診断本文）、rename、formatting、semantic tokens、LSP最大3回指数バックオフ再起動を実装した。
- LSP同期にdidSave/didCloseを追加し、WorkspaceEditの未オープンファイルもアトミック保存するようにした。
- 補完は矢印選択とEnter/Tab確定に対応した。
- P10の外部変更監視、未変更時自動再読込、編集済み時警告、保存競合Confirmを実装した。
- トーストと継続進捗表示、診断/gitの画面外バッジ、ファイル名・変更状態・診断数・git数・位置・カーソル行診断を含むステータス行を実装した。
- 未保存終了とバッファcloseをConfirm化し、選択全行へのindent/outdent/comment toggleを実装した。
- 大容量ファイルへ行カーソル、範囲コピー、単一ファイルストリーミングgrepを追加した。
- F6 diffへ左右フォーカス、編集後再計算、アクティブ側カーソル追従スクロールを追加した。
- PTYリサイズを`TIOCSWINSZ`へ反映し、外部変更監視周期でもUIを更新する。
- マウスサイドボタンはcrossterm 0.28.1がボタン8/9をイベント化せずエラーにすることをソースで確認した。残件と理由を`issue.md`へ記録した。
- 単体テスト49件、`cargo clippy --all-targets -- -D warnings`、`git diff --check`、`cargo deny check`が成功した。
- 実PTYで`cargo run -- DESIGN.md README.md`を起動し、F6ピッカー、左右diff、F4終了、マウスモード/代替画面解除を確認した。
- 追加仕様として`Ctrl+P`コマンドパレットを`DESIGN.md`へ反映し、コマンド名・説明・キーバインドを対象にしたfuzzy検索、候補行へのキー併記、Enter実行を実装した。パレットを開いた左右ペインのフォーカスも復元する。
- コマンドパレット追加後は単体テスト53件、`clippy --all-targets -- -D warnings`、`git diff --check`が成功した。実PTYでも`Ctrl+P`→`diff`→EnterでF6比較ピッカーが開き、F4終了時に端末が復元されることを確認した。

## フェーズ

- [x] P0 ランタイム骨格
- [x] P1 エディタコア
- [x] P2 マルチカーソル＋基本マウス
- [x] P3 ファイル入出力
- [x] P4 オーバーレイ基盤
- [x] P5 検索
- [x] P6 tree-sitter ハイライト
- [x] P7 LSP 基盤
- [x] P8 LSP 機能
- [x] P9 統合ターミナル
- [x] P10 仕上げ（サイドボタンのライブラリ制約は`issue.md`に記録）
