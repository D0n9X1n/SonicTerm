menu-file-new-tab = New Tab
menu-edit-copy = Copy
menu-edit-paste = Paste
prefs-title = Preferences
prefs-category-font = Font
prefs-category-theme = Theme
prefs-category-keymap = Keymap
prefs-category-window = Window
prefs-category-cursor = Cursor
prefs-category-advanced = Advanced
prefs-category-font-description = Choose the typeface and metrics used for terminal text.
prefs-category-theme-description = Pick the color theme and preview terminal chrome.
prefs-category-keymap-description = Select the keyboard shortcut preset.
prefs-category-window-description = Tune window chrome, opacity, blur, and padding.
prefs-category-cursor-description = Adjust cursor shape and blink behavior.
prefs-category-advanced-description = Set shell startup, scrollback, language, and diagnostics.
prefs-theme = Theme
prefs-font-family = Font Family
prefs-font-size = Font Size
prefs-line-height = Line height
prefs-accent = Accent
prefs-open-keymap-file = Open keymap file
prefs-keymap-auto-reload = Sonic watches the platform-default keymap; reselect or restart for custom-named files
prefs-opacity = Opacity
prefs-background-blur = Background blur
prefs-window-decorations = Window decorations
prefs-padding = Padding
prefs-cursor-shape = Cursor shape
prefs-cursor-blink = Blink Cursor
prefs-shell = Shell
prefs-scrollback = Scrollback
prefs-language = Language
prefs-language-auto = Auto
prefs-apply = Apply
prefs-cancel = Cancel
prefs-reset-to-default = Reset to default
prefs-unsaved-changes = Unsaved changes
palette-placeholder = Type a command…
search-placeholder = Find in terminal…
tab-new = New Tab
tab-close = Close Tab
ime-composing = Composing: { $text }
command-new-tab = New Tab
command-close-tab = Close Tab
command-close-pane-or-tab = Close Pane or Tab
command-next-tab = Next Tab
command-prev-tab = Previous Tab
command-activate-tab = Activate Tab { $number }
command-activate-last-tab = Activate Last Tab
command-split-right = Split Pane Right
command-split-down = Split Pane Down
command-close-pane = Close Pane
command-toggle-pane-zoom = Toggle Pane Zoom
command-toggle-broadcast =
    { $scope ->
        [tab] Toggle Broadcast Tab
       *[all] Toggle Broadcast All Tabs
    }
command-focus-pane =
    { $direction ->
        [left] Focus Pane Left
        [right] Focus Pane Right
        [up] Focus Pane Up
       *[down] Focus Pane Down
    }
command-resize-pane-left = Resize Pane Left
command-resize-pane-right = Resize Pane Right
command-resize-pane-up = Resize Pane Up
command-resize-pane-down = Resize Pane Down
command-resize-pane =
    { $direction ->
        [left] Resize Pane Left by { $amount }
        [right] Resize Pane Right by { $amount }
        [up] Resize Pane Up by { $amount }
       *[down] Resize Pane Down by { $amount }
    }
command-copy = Copy to Clipboard
command-copy-mode = Enter Read Only Mode
command-quick-select = Enter Quick Select
command-paste = Paste from Clipboard
command-increase-font-size = Increase Font Size
command-decrease-font-size = Decrease Font Size
command-reset-font-size = Reset Font Size
command-increase-font-weight = Increase Font Weight (Bolder)
command-decrease-font-weight = Decrease Font Weight (Thinner)
command-reset-font-weight = Reset Font Weight to Config
command-save-settings = Save Current Settings
command-apply-theme = Apply Theme: { $name }
command-toggle-tab-bar = Toggle Tab Bar
command-rename-tab = Rename Active Tab
command-tab-color = Update Tab Color
command-new-window = New Window
command-move-tab-window = Move Tab to New Window
command-fullscreen = Toggle Fullscreen
command-quit = Quit SonicTerm
command-search = Open Search
command-palette = Open Command Palette
command-edit-config = Edit sonicterm.toml
command-edit-keymap = Edit keymap.toml
command-check-updates = Check for Updates
command-scroll =
    { $target ->
        [line-up] Scroll Line Up
        [line-down] Scroll Line Down
        [page-up] Scroll Page Up
        [page-down] Scroll Page Down
        [top] Scroll To Top
       *[bottom] Scroll To Bottom
    }
command-prev-prompt = Scroll to Previous Prompt
command-next-prompt = Scroll to Next Prompt
command-reload-config = Reload Config
command-ssh-pane = Open SSH Pane: { $target }
command-category-tabs = Tabs
command-category-panes = Panes
command-category-clipboard = Clipboard
command-category-appearance = Appearance
command-category-window = Window
command-category-navigation = Navigation
command-category-settings = Settings
palette-go-to-tab = Go to Tab { $number }: { $title }
palette-tabs-placeholder = Search tabs…
palette-tabs-empty = No tabs found
palette-tabs-hint = Search a tab title or position
palette-tabs-footer = All tabs · ↑↓ navigate · ↵ switch · esc close
command-disabled-window = No terminal window
command-disabled-tab = No active tab
command-disabled-pane = No active pane
command-disabled-selection = No text selected
command-disabled-readonly = Unavailable in READONLY mode
command-disabled-tab-index = Tab no longer exists
command-disabled-neighbor = No pane in that direction
palette-search-placeholder = Search commands, settings, shortcuts…
palette-rename-placeholder = New tab title…
palette-no-matches = No commands found
palette-empty-hint = Try settings, split, font, shortcut
palette-rename-footer = ↵ rename · esc cancel
palette-color-footer = ↑↓ choose color · ↵ apply · esc cancel
palette-color-title = Color for { $title }
palette-command-footer =
    { $count } { $count-kind ->
        [one] command
       *[other] commands
    } · ↑↓ navigate · ↵ run · esc close
