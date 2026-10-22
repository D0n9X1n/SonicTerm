path-error-copy-again = このメッセージの表示中に同じリンクをもう一度クリックすると、パスをコピーします。
path-error-title = 対象を開けません
path-error-missing = ファイルまたはフォルダーが見つかりません
path-error-blocked = パスにアクセスできないか、安全ポリシーで禁止されています
path-error-pending = 対象を確認中です。しばらくしてから再試行してください
path-error-disabled = ローカル対象の操作は無効です
path-error-busy = ファイルマネージャーの要求が混雑しています。再試行してください
path-error-worker = ファイルマネージャーのワーカーを利用できません
path-error-copied = パスまたは元の対象をクリップボードにコピーしました
path-error-copy-failed = パスをクリップボードにコピーできませんでした
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
command-toggle-status-line = ステータスラインの表示を切り替え
command-toggle-status-line-sensitive = ステータスラインの機密内容を切り替え
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
command-show-memory-usage = メモリ使用量
command-copy-memory-report = メモリレポートをコピー
command-set-default-handlers-mac = SonicTerm をスクリプト、実行ファイル、ターミナルリンクの既定に設定…
command-set-default-handlers-linux = SonicTerm をシェルスクリプトの既定に設定…
command-set-default-handlers-windows = 既定のアプリで SonicTerm を選択…
command-set-default-handlers-search = 既定 ハンドラー ターミナル このアプリケーションで開く ファイルの種類 リンク
palette-confirm-footer = ↑↓ 選択 · ↵ 確定 · esc キャンセル
default-handlers-title-change = SonicTerm が次の既定になります:
default-handlers-title-windows = 設定 › 既定のアプリ を開きます。そこで次の項目に SonicTerm を選択してください:
default-handlers-apply = 適用
default-handlers-open-settings = 既定のアプリを開く
default-handlers-cancel = キャンセル
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
palette-about-title = SonicTerm について
palette-about-search = SonicTerm について バージョン ビルド
palette-search-placeholder = コマンド、設定、ショートカットを検索…
palette-rename-placeholder = 新しいタブ名…
palette-no-matches = コマンドが見つかりません
palette-empty-hint = 設定、分割、フォント、ショートカットを試してください
palette-rename-footer = ↵ 名前を変更 · esc キャンセル
palette-color-footer = ↑↓ 色を選択 · ↵ 適用 · esc キャンセル
palette-color-title = { $title } の色
palette-command-footer = { $count }件のコマンド · ↑↓ 移動 · ↵ 実行 · esc 閉じる
memory-report-window-title = メモリ使用量
memory-report-copied = メモリレポートをコピーしました

# Status line strip: names are dim labels, values follow them; spoken text has no symbols.
status-name-mem = メモリ
status-name-scroll = スクロール
status-name-link = リンク
status-name-path = パス
status-name-config = 設定
status-name-theme = テーマ
status-name-keymap = キーマップ
status-name-render = 描画
status-name-gpu = GPU
status-value-na = なし
status-value-error = エラー
status-value-line = { $line } 行目
status-value-software = ソフトウェア
status-value-software-gdi = ソフトウェア (GDI)
status-value-software-adapter = ソフトウェアアダプター
status-value-software-forced = ソフトウェア (強制)
status-value-recovering = 復旧中
status-value-bottom = 最下部
status-value-normal = 通常
status-chip-copy = コピー
status-chip-mouse = マウス
status-value-reveal = 表示
status-value-open-folder = フォルダーを開く
status-value-checking = 確認中
status-value-not-found = 見つかりません
status-value-blocked = ブロック
status-spoken-rising = 増加
status-spoken-falling = 減少
status-spoken-lines-up = { $count } 行上
status-spoken-copy-mode = コピーモード
status-spoken-broadcast = { $count } 個のペインに一斉送信
status-spoken-mouse-captured = マウスはプログラムが使用中
status-spoken-alerts = 通知
status-spoken-health = 状態
status-spoken-mem = メモリ
status-spoken-mode = モード
status-spoken-input = 入力
status-spoken-clip = クリップボード
status-spoken-path-check = パス確認
status-spoken-link = ポインター下のリンク
status-spoken-paste = 貼り付け
status-spoken-sel = 選択範囲
status-spoken-ime = 入力メソッド
status-spoken-scroll = スクロール位置
status-spoken-cpu = CPU
status-spoken-frame = フレーム時間
status-spoken-out = 出力速度
status-spoken-tab-mem = タブのメモリ
status-spoken-glyphs = 欠けている字形
status-spoken-history = 履歴
status-spoken-font = フォント
status-spoken-zoom = ズーム
status-spoken-size = サイズ
status-reason-no-directory = シェルが報告したディレクトリがありません
status-reason-alt-screen = 代替画面では解決しません
status-value-label-not-checked = 表示文字は未確認
