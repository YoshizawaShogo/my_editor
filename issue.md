# 問題点・確認事項

## 未解決
- GNOME Terminal実機で、行頭Tabおよびインデント後Enterの中間描画が露出しないことを確認する。論理バッファは回帰テスト上、Tab後=`4 spaces`、Enter後=`4 spaces + 1 newline + 4 spaces`で余分な改行・実タブがない。
