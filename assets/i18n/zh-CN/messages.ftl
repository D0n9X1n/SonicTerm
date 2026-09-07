menu-file-new-tab = 新建标签页
menu-edit-copy = 复制
menu-edit-paste = 粘贴
prefs-title = 偏好设置
prefs-category-font = 字体
prefs-category-theme = 主题
prefs-category-keymap = 键位映射
prefs-category-window = 窗口
prefs-category-cursor = 光标
prefs-category-advanced = 高级
prefs-category-font-description = 选择终端文本使用的字体和度量。
prefs-category-theme-description = 选择配色主题并预览终端界面。
prefs-category-keymap-description = 选择键盘快捷键预设。
prefs-category-window-description = 调整窗口装饰、不透明度、模糊和内边距。
prefs-category-cursor-description = 调整光标形状和闪烁行为。
prefs-category-advanced-description = 设置 Shell 启动、回滚缓冲、语言和诊断。
prefs-theme = 主题
prefs-font-family = 字体
prefs-font-size = 字号
prefs-line-height = 行高
prefs-accent = 强调色
prefs-open-keymap-file = 打开键位映射文件
prefs-keymap-auto-reload = Sonic 会监视平台默认 keymap；自定义名称文件请重新选择或重启
prefs-opacity = 不透明度
prefs-background-blur = 背景模糊
prefs-window-decorations = 窗口装饰
prefs-padding = 内边距
prefs-cursor-shape = 光标形状
prefs-cursor-blink = 光标闪烁
prefs-shell = 命令行 Shell
prefs-scrollback = 回滚缓冲
prefs-language = 语言
prefs-language-auto = 自动
prefs-apply = 应用
prefs-cancel = 取消
prefs-reset-to-default = 重置为默认值
prefs-unsaved-changes = 未保存的更改
palette-placeholder = 输入命令…
search-placeholder = 在终端中查找…
tab-new = 新建标签页
tab-close = 关闭标签页
ime-composing = 输入中：{ $text }
command-new-tab = 新建标签页
command-close-tab = 关闭标签页
command-close-pane-or-tab = 关闭窗格或标签页
command-next-tab = 下一个标签页
command-prev-tab = 上一个标签页
command-activate-tab = 切换到标签页 { $number }
command-activate-last-tab = 切换到最后一个标签页
command-split-right = 向右分屏
command-split-down = 向下分屏
command-close-pane = 关闭窗格
command-toggle-pane-zoom = 切换窗格缩放
command-toggle-broadcast =
    { $scope ->
        [tab] 切换当前标签页广播
       *[all] 切换所有标签页广播
    }
command-focus-pane =
    { $direction ->
        [left] 焦点移向左侧窗格
        [right] 焦点移向右侧窗格
        [up] 焦点移向上方窗格
       *[down] 焦点移向下方窗格
    }
command-resize-pane-left = 向左调整窗格
command-resize-pane-right = 向右调整窗格
command-resize-pane-up = 向上调整窗格
command-resize-pane-down = 向下调整窗格
command-resize-pane =
    { $direction ->
        [left] 向左调整窗格 { $amount } 步
        [right] 向右调整窗格 { $amount } 步
        [up] 向上调整窗格 { $amount } 步
       *[down] 向下调整窗格 { $amount } 步
    }
command-copy = 复制到剪贴板
command-copy-mode = 进入只读模式
command-quick-select = 进入快速选择
command-paste = 从剪贴板粘贴
command-increase-font-size = 增大字号
command-decrease-font-size = 减小字号
command-reset-font-size = 重置字号
command-increase-font-weight = 增加字重（更粗）
command-decrease-font-weight = 减少字重（更细）
command-reset-font-weight = 将字重重置为配置值
command-save-settings = 保存当前设置
command-apply-theme = 应用主题：{ $name }
command-toggle-tab-bar = 切换标签栏显示
command-rename-tab = 重命名当前标签页
command-tab-color = 更改标签页颜色
command-new-window = 新建窗口
command-move-tab-window = 将标签页移到新窗口
command-fullscreen = 切换全屏
command-quit = 退出 SonicTerm
command-search = 打开搜索
command-palette = 打开命令面板
command-edit-config = 编辑 sonicterm.toml
command-edit-keymap = 编辑 keymap.toml
command-check-updates = 检查更新
command-scroll =
    { $target ->
        [line-up] 向上滚动一行
        [line-down] 向下滚动一行
        [page-up] 向上滚动一页
        [page-down] 向下滚动一页
        [top] 滚动到顶部
       *[bottom] 滚动到底部
    }
command-prev-prompt = 滚动到上一个提示符
command-next-prompt = 滚动到下一个提示符
command-reload-config = 重载配置
command-ssh-pane = 打开 SSH 窗格：{ $target }
command-category-tabs = 标签页
command-category-panes = 窗格
command-category-clipboard = 剪贴板
command-category-appearance = 外观
command-category-window = 窗口
command-category-navigation = 导航
command-category-settings = 设置
palette-go-to-tab = 切换到标签页 { $number }：{ $title }
palette-tabs-placeholder = 搜索标签页…
palette-tabs-empty = 未找到标签页
palette-tabs-hint = 搜索标签页标题或位置
palette-tabs-footer = 所有标签页 · ↑↓ 导航 · ↵ 切换 · esc 关闭
command-disabled-window = 没有终端窗口
command-disabled-tab = 没有活动标签页
command-disabled-pane = 没有活动窗格
command-disabled-selection = 未选择文本
command-disabled-readonly = 只读模式下不可用
command-disabled-tab-index = 标签页已不存在
command-disabled-neighbor = 该方向没有窗格
palette-search-placeholder = 搜索命令、设置、快捷键…
palette-rename-placeholder = 新标签页标题…
palette-no-matches = 未找到命令
palette-empty-hint = 试试设置、分屏、字体、快捷键
palette-rename-footer = ↵ 重命名 · esc 取消
palette-color-footer = ↑↓ 选择颜色 · ↵ 应用 · esc 取消
palette-color-title = { $title } 的颜色
palette-command-footer = { $count } 个命令 · ↑↓ 导航 · ↵ 执行 · esc 关闭
