# Keymap

現在のキー体系をモードごとに整理した一覧です。

## Normal Mode

| Key | Action | Notes |
| --- | --- | --- |
| `a` | Insert mode に入る | カーソルの右側に追記 |
| `h` | Insert mode に入る | カーソル位置で挿入 |
| `i` | 上へ移動 | |
| `j` | 左へ移動 | |
| `k` | 下へ移動 | |
| `l` | 右へ移動 | |
| `Home` | 行頭へ移動 | |
| `End` | 行末へ移動 | |
| `Ctrl-D` | 半ページ下へ移動 | |
| `Ctrl-U` | 半ページ上へ移動 | |
| `b` | jump history を戻る | back |
| `B` | jump history を進む | forward |
| `u` | Undo | |
| `U` | Redo | |
| `p` | Paste after | |
| `P` | Paste before | |
| `o` | 現在行の下に行を開いて Insert | |
| `%` | 対応括弧へ移動 | `()`, `{}`, `[]` |
| `r` | 直前の replayable action を再実行 | |
| `R` | 直前の replayable action を逆方向に再実行 | |
| `f{char}` | 次の文字へ移動 | |
| `F{char}` | 前の文字へ移動 | |
| `t{char}` | 次の文字の直前へ移動 | |
| `T{char}` | 前の文字の直後へ移動 | |
| `cc` | 現在行を change | |
| `dd` | 現在行を delete | |
| `yy` | 現在行を yank | |
| `cf/F/t/T` | find/till 範囲を change | |
| `df/F/t/T` | find/till 範囲を delete | |
| `yf/F/t/T` | find/till 範囲を yank | |
| `ci` | syntax selection を開始 | `i` で範囲拡大、`Enter` で確定 |
| `di` | syntax selection を開始 | `i` で範囲拡大、`Enter` で確定 |
| `yi` | syntax selection を開始 | `i` で範囲拡大、`Enter` で確定 |
| `gt` | 先頭へ移動 | top |
| `gT` | 末尾へ移動 | bottom |
| `gg` | 次の Git hunk へ移動 | |
| `gG` | 前の Git hunk へ移動 | |
| `gw` | 次の warning/error へ移動 | |
| `gW` | 前の warning/error へ移動 | |
| `ge` | 次の error へ移動 | |
| `gE` | 前の error へ移動 | |
| `gf` | 次の検索結果へ移動 | |
| `gF` | 前の検索結果へ移動 | |
| `gd` | 定義へジャンプ | LSP |
| `gD` | 宣言へジャンプ | LSP |
| `gi` | 実装へジャンプ | LSP |
| `gr` | 参照一覧を開く | LSP |
| `e` | diagnostic prefix | |
| `ed` | 現在行の diagnostic 詳細 | popup |
| `ew` | open buffers の warning/error 一覧 | scratch |
| `ee` | open buffers の error 一覧 | scratch |
| `eW` | workspace の warning/error 一覧 | scratch |
| `eE` | workspace の error 一覧 | scratch |
| `K` | hover を開く | LSP |
| `F2` | rename を開く | LSP |
| `Ctrl-G` | Go to line を開く | |
| `Ctrl-P` | file picker を開く | 再押下で閉じる |
| `Ctrl-F` | search を開く | 再押下で scope 循環 |
| `Ctrl-H` | replace を開く | 再押下で scope 循環 |
| `Ctrl-S` | 保存 | |
| `Ctrl-W` | 現在 buffer を閉じる | |
| `Ctrl-L` | split を進める / focus 移動 | |
| `Ctrl-O` | focused pane を 1 画面化 | |
| `Ctrl-@` / `Ctrl-Space` | shell pane を開閉 | |
| `Ctrl-J` | scratch target を開く | scratch buffer 上 |
| `Enter` | scratch target を開く | scratch buffer 上 |
| `Esc` / `Ctrl-C` | pending command をキャンセル | scratch buffer 上では閉じる |
| `Ctrl-Q` | 終了 | |

## Insert Mode

| Key | Action | Notes |
| --- | --- | --- |
| `Esc` | Normal mode に戻る | |
| `Ctrl-C` | Normal mode に戻る | |
| `jj` | Normal mode に戻る | |
| `Up` | 上へ移動 | completion を閉じる |
| `Left` | 左へ移動 | completion を閉じる |
| `Down` | 下へ移動 | completion を閉じる |
| `Right` | 右へ移動 | completion を閉じる |
| `Home` | 行頭へ移動 | completion を閉じる |
| `End` | 行末へ移動 | completion を閉じる |
| `Enter` | 改行 | completion 確定には使わない |
| `Ctrl-J` | 改行 | |
| `Ctrl-M` | 改行 | |
| `Tab` | 先頭の補完候補を確定 | completion popup がある時 |
| `Backspace` | 後退削除 | completion 更新 |
| `Delete` | 前方削除 | completion 更新 |
| `Ctrl-H` | 後退削除 | |
| `Ctrl-D` | 前方削除 | |
| `Ctrl-S` | 保存して Normal mode に戻る | |
| 文字入力 | 挿入 | Rust では自動補完候補を表示 |

## Shell Mode

| Key | Action | Notes |
| --- | --- | --- |
| `Ctrl-L` | pane focus / layout を切り替える | shell から editor に戻る時にも使う |
| `Ctrl-O` | shell pane を 1 画面化 | shell focus 中 |
| `Ctrl-@` / `Ctrl-Space` | shell pane を開閉 / 再起動 | shell 終了後も再起動可能 |
| `Esc` | editor 側へ focus を戻す | shell には送らない |
| その他のキー | shell にそのまま送る | `Ctrl-W`, `Ctrl-A`, `Ctrl-C`, `Ctrl-D` など含む |

## Popup / Input Modes

### Search

| Key | Action |
| --- | --- |
| `Ctrl-F` | scope を `file -> buffers -> project` で循環 |
| `Enter` / `Ctrl-J` / `Ctrl-M` | 検索を実行 |
| `Backspace` / `Ctrl-H` | 1 文字削除 |
| `Esc` / `Ctrl-C` | 閉じる |

### Replace

| Key | Action |
| --- | --- |
| `Ctrl-H` | scope を `file -> buffers -> project` で循環 |
| `Tab` | `from` と `to` を切り替える |
| `Enter` / `Ctrl-J` / `Ctrl-M` | replace-all を実行 |
| `Backspace` / `Ctrl-H` | 現在欄を 1 文字削除 |
| `Esc` / `Ctrl-C` | 閉じる |

### Picker

| Key | Action |
| --- | --- |
| `Ctrl-P` | 閉じる |
| `Enter` / `Ctrl-J` | 先頭候補を開く |
| `Backspace` | 1 文字削除 |
| `w` | 閉じる |
| `Esc` / `Ctrl-C` | 閉じる |
| 文字入力 | query 更新 |

### Go To Line

| Key | Action |
| --- | --- |
| `Enter` / `Ctrl-J` / `Ctrl-M` | 確定 |
| `Backspace` / `Ctrl-H` | 1 文字削除 |
| `Esc` / `Ctrl-C` | 閉じる |
| 数字 | 入力 |

### Rename

| Key | Action |
| --- | --- |
| `Enter` / `Ctrl-J` | 確定 |
| `Backspace` / `Ctrl-H` | 1 文字削除 |
| `Esc` / `Ctrl-C` | 閉じる |
| 文字入力 | 入力 |

### Diagnostic Popup

| Key | Action |
| --- | --- |
| 任意のキー | 閉じる |
| `Esc` / `Ctrl-C` | 閉じる |
| `w` | warning/error 一覧 |
| `e` | error 一覧 |
| `W` | workspace warning/error 一覧 |
| `E` | workspace error 一覧 |

### Selection Input

| Key | Action |
| --- | --- |
| `i` | 次の enclosing range に拡大 |
| `Enter` / `Ctrl-J` / `Ctrl-M` | 現在 range を確定 |
| `Esc` / `Ctrl-C` | 中断 |

