# 問題点・確認事項

## 未解決

- F6 diff は行数の積が100万以下ならLCSを厳密計算し、それ以上はメモリ上限を守るため行番号対応へフォールバックする。大規模diffのキャッシュ・増分再計算は未実装。
- `crossterm` 0.28.1のUnixパーサーはボタン8/9を明示的にエラーにするため、マウスサイドボタンのundo/redoは取得できない。対応には`EventStream`を置換する端末入力パーサーが必要で、通常のキー・マウス入力全体への回帰リスクがある。キーボードの`Ctrl+Z`/`Ctrl+Y`は利用可能。
- `mmap`中のファイルが外部からtruncateされた場合はSIGBUSの可能性がある。設計どおり読み取り専用ビューでは許容している。
- undo永続履歴はv1では容量上限・GCを持たず、長期利用時に`~/my_editor_undo_history`が肥大化し得る。
- 置換は検索結果一覧とDirectory最終確認を持つが、before/afterを並べる専用プレビューと各マッチの個別選択解除は未実装。
- LSP `WorkspaceEdit`はTextEditを開いているバッファと未オープンファイルへ適用する。Create/Rename/DeleteFile操作、percent-encodingされたfile URIの復号は未対応。
- LSP `didChange`は安全な全文同期であり、差分同期への最適化は未実装。
- `cargo deny check` は成功するが、推移依存の重複バージョンと未使用 license allowance を警告する。現時点では advisory・license・source 違反はない。

## 実装上の注意

- raw mode・代替画面・マウスキャプチャは、正常終了と panic の両方で必ず解除する。
- `Editor::update` から IO を行わず、すべて `Effect` としてランタイムへ返す。
- エディタ状態はメインループが単一所有し、`Arc<Mutex<Editor>>` を導入しない。

## 解決済み

- `Ctrl+P`のコマンドパレットで、diff/find等を名前・説明・キーから検索し、キーバインドを確認しながら実行できる。
- 連続文字入力は750ms以内を同一revisionへまとめ、移動等でグループを切る。
- F6 diffは左右ペインのフォーカス・編集・カーソル追従・再計算に対応した。
- 保存時のmtime/size競合検出、上書きConfirm、2秒間隔の外部変更監視、自動再読込を実装した。
- ファイルピッカーの直接パス入力（`~`、相対/絶対パス、ワークスペース内新規ファイル）を実装した。
- 検索のcase/whole-word/regex、include/exclude、ignore/hidden、Directory置換確認を実装した。
- tree-sitterは`InputEdit`と旧Treeを使う増分パースへ変更した。
- LSP hover/診断集約、rename、formatting、semantic tokens、最大3回の指数バックオフ再起動を実装した。
- PTYリサイズ時の`TIOCSWINSZ`更新を実装した。
- 現時点で実装を停止する仕様上の疑問点はない。
- 旧モーダル仕様だった `docs/keymap.md` を、現行 `DESIGN.md` §7.4 の非モーダル仕様と F6 diff 仕様に同期した。
