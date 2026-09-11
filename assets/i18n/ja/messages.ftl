link-preview-unavailable = このリンクは開けません
path-preview-open = 開く
path-preview-reveal = 場所を表示するだけで実行しません
menu-file-new-tab = 新しいタブ
menu-edit-copy = コピー
menu-edit-paste = 貼り付け
prefs-title = 環境設定
prefs-category-font = フォント
prefs-category-theme = テーマ
prefs-category-keymap = キーマップ
prefs-category-window = ウィンドウ
prefs-category-cursor = カーソル
prefs-category-advanced = 詳細
prefs-category-font-description = ターミナル文字の書体とメトリクスを選択します。
prefs-category-theme-description = 配色テーマを選び、ターミナルのクロームをプレビューします。
prefs-category-keymap-description = キーボードショートカットのプリセットを選択します。
prefs-category-window-description = ウィンドウ装飾、不透明度、ぼかし、余白を調整します。
prefs-category-cursor-description = カーソル形状と点滅動作を調整します。
prefs-category-advanced-description = シェル起動、スクロールバック、言語、診断を設定します。
prefs-theme = テーマ
prefs-font-family = フォント
prefs-font-size = フォントサイズ
prefs-line-height = 行の高さ
prefs-accent = アクセント
prefs-open-keymap-file = キーマップファイルを開く
prefs-keymap-auto-reload = Sonic は既定キーマップを監視します。独自名のファイルは再選択または再起動してください
prefs-opacity = 不透明度
prefs-background-blur = 背景ぼかし
prefs-window-decorations = ウィンドウ装飾
prefs-padding = 余白
prefs-cursor-shape = カーソル形状
prefs-cursor-blink = カーソル点滅
prefs-shell = シェル
prefs-scrollback = スクロールバック
prefs-language = 言語
prefs-language-auto = 自動
prefs-apply = 適用
prefs-cancel = キャンセル
prefs-reset-to-default = 既定に戻す
prefs-unsaved-changes = 未保存の変更
palette-placeholder = コマンドを入力…
search-placeholder = ターミナル内を検索…
tab-new = 新しいタブ
tab-close = タブを閉じる
ime-composing = 入力中: { $text }
command-new-tab = 新しいタブ
command-close-tab = タブを閉じる
command-close-pane-or-tab = ペインまたはタブを閉じる
command-next-tab = 次のタブ
command-prev-tab = 前のタブ
command-activate-tab = タブ { $number } に切り替え
command-activate-last-tab = 最後のタブに切り替え
command-split-right = 右にペインを分割
command-split-down = 下にペインを分割
command-close-pane = ペインを閉じる
command-toggle-pane-zoom = ペインの拡大を切り替え
command-toggle-broadcast =
    { $scope ->
        [tab] 現在のタブへの同時送信を切り替え
       *[all] すべてのタブへの同時送信を切り替え
    }
command-focus-pane =
    { $direction ->
        [left] 左のペインにフォーカス
        [right] 右のペインにフォーカス
        [up] 上のペインにフォーカス
       *[down] 下のペインにフォーカス
    }
command-resize-pane-left = ペインを左に調整
command-resize-pane-right = ペインを右に調整
command-resize-pane-up = ペインを上に調整
command-resize-pane-down = ペインを下に調整
command-resize-pane =
    { $direction ->
        [left] ペインを左に { $amount } ステップ調整
        [right] ペインを右に { $amount } ステップ調整
        [up] ペインを上に { $amount } ステップ調整
       *[down] ペインを下に { $amount } ステップ調整
    }
command-copy = クリップボードにコピー
command-copy-mode = 読み取り専用モードに入る
command-quick-select = クイック選択に入る
command-paste = クリップボードから貼り付け
command-increase-font-size = フォントサイズを大きくする
command-decrease-font-size = フォントサイズを小さくする
command-reset-font-size = フォントサイズをリセット
command-increase-font-weight = フォントを太くする
command-decrease-font-weight = フォントを細くする
command-reset-font-weight = フォントの太さを設定値に戻す
command-save-settings = 現在の設定を保存
command-apply-theme = テーマを適用：{ $name }
command-toggle-tab-bar = タブバーの表示を切り替え
command-rename-tab = 現在のタブの名前を変更
command-rename-window = ウィンドウの名前を変更
palette-window-name-placeholder = ウィンドウ名（空欄でリセット）…
palette-window-name-footer = ↵ 保存 · 空欄でリセット · esc キャンセル
palette-window-name-controls = 名前に制御文字や改行は使えません
palette-window-name-too-long = 名前は 128 文字以内にしてください
command-tab-color = タブの色を変更
command-new-window = 新しいウィンドウ
command-move-tab-window = タブを新しいウィンドウに移動
command-fullscreen = 全画面表示を切り替え
command-quit = SonicTerm を終了
command-search = 検索を開く
command-palette = コマンドパレットを開く
command-edit-config = sonicterm.toml を編集
command-edit-keymap = keymap.toml を編集
command-check-updates = 更新を確認
command-scroll =
    { $target ->
        [line-up] 1 行上にスクロール
        [line-down] 1 行下にスクロール
        [page-up] 1 ページ上にスクロール
        [page-down] 1 ページ下にスクロール
        [top] 先頭にスクロール
       *[bottom] 末尾にスクロール
    }
command-prev-prompt = 前のプロンプトへスクロール
command-next-prompt = 次のプロンプトへスクロール
command-reload-config = 設定を再読み込み
command-ssh-pane = SSH ペインを開く：{ $target }
command-category-tabs = タブ
command-category-panes = ペイン
command-category-clipboard = クリップボード
command-category-appearance = 外観
command-category-window = ウィンドウ
command-category-navigation = ナビゲーション
command-category-settings = 設定
palette-go-to-tab = タブ { $number } に移動：{ $title }
palette-tabs-placeholder = タブを検索…
palette-tabs-empty = タブが見つかりません
palette-tabs-hint = タブのタイトルまたは位置で検索
palette-tabs-footer = すべてのタブ · ↑↓ 移動 · ↵ 切り替え · esc 閉じる
command-disabled-window = ターミナルウィンドウがありません
command-disabled-tab = アクティブなタブがありません
command-disabled-pane = アクティブなペインがありません
command-disabled-selection = テキストが選択されていません
command-disabled-readonly = 読み取り専用モードでは使用できません
command-disabled-tab-index = タブは存在しません
command-disabled-neighbor = その方向にペインがありません
palette-search-placeholder = コマンド、設定、ショートカットを検索…
palette-rename-placeholder = 新しいタブ名…
palette-no-matches = コマンドが見つかりません
palette-empty-hint = 設定、分割、フォント、ショートカットを試してください
palette-rename-footer = ↵ 名前を変更 · esc キャンセル
palette-color-footer = ↑↓ 色を選択 · ↵ 適用 · esc キャンセル
palette-color-title = { $title } の色
palette-command-footer = { $count }件のコマンド · ↑↓ 移動 · ↵ 実行 · esc 閉じる
