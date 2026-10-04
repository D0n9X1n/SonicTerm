//! Wezterm-style tab title formatter.
//!
//! Produces a string of the form `#N{icon} {parent}/{leaf}` where:
//! - `N`  — 1-based tab index (the user-visible position in the bar).
//! - `{icon}` — a Nerd Font glyph picked from the foreground process name
//!   (Zsh, Vim/Neovim, SSH, Claude Code, Copilot CLI, …). Falls back to a
//!   folder glyph when the process is unknown but a cwd is reported.
//! - `{parent}/{leaf}` — the last two path components of the pane's cwd.
//!   A single-component path (e.g. `/tmp`) shows as just that component.
//!
//! OSC 0/2 titles take priority for rmux/tmux/screen sessions; other processes
//! use them only when no CWD is available. User title overrides live in the tab state.

/// Wezterm's "fancy mode" vertical separator drawn between tabs.
/// U+2502 BOX DRAWINGS LIGHT VERTICAL, followed by a single space of
/// padding (~6–8px at typical monospace cell widths) to keep the
/// separator visually clear of the next tab's title.
pub const TAB_SEPARATOR_PREFIX: &str = "\u{2502} ";

/// Build the on-screen label for a tab. Mirrors wezterm fancy-mode:
/// every tab except the first is prefixed by `│ ` so a thin divider
/// appears between adjacent tab titles. Callers that render the
/// separator in a distinct color should use [`TAB_SEPARATOR_PREFIX`]
/// directly and split the returned string on its length — or, more
/// commonly, look at the tab `index` themselves.
#[must_use]
pub fn tab_display_label(index: usize, title: &str) -> String {
    if index == 0 {
        title.to_string()
    } else {
        // When: `index` is nonzero, the tab is not first, so it carries the separator prefix.
        format!("{TAB_SEPARATOR_PREFIX}{title}")
    }
}

/// Format a tab title in wezterm style. See module docs for the contract.
///
/// All inputs are optional so that the function works regardless of which
/// signals the pty has produced so far. The return value is always a
/// non-empty `String` (a bare `#N` shell fallback is the worst case).
#[must_use]
pub fn format_tab_title(
    index: usize,
    cwd: Option<&str>,
    process: Option<&str>,
    raw_title: Option<&str>,
) -> String {
    let tab_number = index + 1;
    let icon = icon_for_process(process, cwd.is_some());

    let raw_title = raw_title.map(str::trim).filter(|trimmed| !trimmed.is_empty());
    let multiplexer = process.is_some_and(|name| {
        ["rmux", "tmux", "screen"].iter().any(|mux| name.eq_ignore_ascii_case(mux))
    });
    let body = if let Some(title) = raw_title.filter(|_| multiplexer) {
        title.to_string()
    } else if let Some(directory) = cwd {
        // When: cwd is available without a multiplexer title, preserve the ordinary shell's directory label.
        cwd_two_components(directory)
    } else if let Some(osc_title) = raw_title {
        // When: `cwd` is absent but `raw_title` has text, the OSC title names the session.
        osc_title.to_string()
    } else {
        // When: neither `cwd` nor `raw_title` is set, the body falls back to a bare shell label.
        "shell".to_string()
    };

    format!("#{tab_number} {icon} {body}")
}

/// Every glyph [`format_tab_title`] can put in a title's icon slot: each process family's Nerd
/// Font icon, the folder fallback and the generic terminal fallback. The glyph working-set helper
/// measures these in the tab-title strike, so a tab's icon is never a tile it did not count; a
/// unit test pins this list to the mapping's literals.
pub const PROGRAM_ICONS: &[char] = &[
    '\u{F0674}',
    '\u{F4B8}',
    '\u{E84F}',
    '\u{E760}',
    '\u{EE41}',
    '\u{E691}',
    '\u{EBC7}',
    '\u{EBC4}',
    '\u{E62B}',
    '\u{E8DA}',
    '\u{E7CF}',
    '\u{E838}',
    '\u{F08C0}',
    '\u{F0574}',
    '\u{EB4C}',
    '\u{F1D3}',
    '\u{F470}',
    '\u{E7EB}',
    '\u{F1617}',
    '\u{F0320}',
    '\u{E724}',
    '\u{E738}',
    '\u{E82C}',
    '\u{E7F2}',
    '\u{E739}',
    '\u{E73D}',
    '\u{E783}',
    '\u{E826}',
    '\u{E755}',
    '\u{E8EF}',
    '\u{E77F}',
    '\u{E719}',
    '\u{E71E}',
    '\u{E865}',
    '\u{E8EC}',
    '\u{E7C0}',
    '\u{E76F}',
    '\u{E7B0}',
    '\u{E866}',
    '\u{F1323}',
    '\u{E794}',
    '\u{F0774}',
    '\u{E81D}',
    '\u{E7FB}',
    '\u{E8BD}',
    '\u{E723}',
    '\u{E873}',
    '\u{E7AD}',
    '\u{E754}',
    '\u{E7F1}',
    '\u{E792}',
    '\u{E8D3}',
    '\u{E83C}',
    '\u{E76E}',
    '\u{E704}',
    '\u{E828}',
    '\u{E76D}',
    '\u{E7C4}',
    '\u{E7A4}',
    '\u{F07B}',
    '\u{F489}',
];

/// Pick the Nerd Font glyph for a process name. Returns the folder icon
/// when `has_cwd` is true and the process is unknown / absent. Returns a
/// terminal icon when neither is known.
fn icon_for_process(process: Option<&str>, has_cwd: bool) -> char {
    if let Some(process_name) = process {
        // When: `process` reports a name, try its command-specific glyph before cwd and shell fallbacks.

        // When: `process_name` is lowercased, each known command maps to its own Nerd Font glyph; unlisted
        // names fall through to the cwd and terminal glyphs below.
        match process_name.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => return '\u{F0674}', // md-creation
            "copilot" | "github-copilot" | "github-copilot-cli" => {
                return '\u{F4B8}'; // oct-copilot
            }
            "zsh" => return '\u{E84F}',                 // dev-ohmyzsh
            "bash" => return '\u{E760}',                // dev-bash
            "fish" => return '\u{EE41}',                // fa-fish
            "sh" | "dash" => return '\u{E691}',         // seti-shell
            "pwsh" | "powershell" => return '\u{EBC7}', // cod-terminal-powershell
            "cmd" => return '\u{EBC4}',                 // cod-terminal-cmd
            "nvim" | "vim" | "vi" | "nvi" => return '\u{E62B}', // custom-vim
            "code" | "code-insiders" | "codium" | "vscodium" => {
                return '\u{E8DA}'; // dev-vscode
            }
            "emacs" | "emacsclient" => return '\u{E7CF}', // dev-emacs
            "nano" => return '\u{E838}',                  // dev-nano
            "ssh" | "mosh" => return '\u{F08C0}',         // md-ssh
            "rmux" | "tmux" => return '\u{F0574}',        // md-view-quilt: a split-pane layout
            "screen" => return '\u{EB4C}',                // cod-screen-full
            "git" | "lazygit" | "tig" => return '\u{F1D3}', // fa-git
            "gh" | "hub" => return '\u{F470}',            // oct-logo-github
            "glab" => return '\u{E7EB}',                  // dev-gitlab
            "cargo" | "rustc" | "rust-analyzer" => return '\u{F1617}', // md-language-rust
            "python" | "python3" | "ipython" | "pip" | "pip3" => {
                return '\u{F0320}'; // md-language-python
            }
            "go" | "gofmt" | "gopls" => return '\u{E724}', // dev-go
            "java" | "javac" => return '\u{E738}',         // dev-java
            "mvn" | "mvnw" => return '\u{E82C}',           // dev-maven
            "gradle" | "gradlew" => return '\u{E7F2}',     // dev-gradle
            "ruby" | "irb" | "bundle" | "bundler" | "gem" | "rails" => {
                return '\u{E739}'; // dev-ruby
            }
            "php" | "php-fpm" => return '\u{E73D}', // dev-php
            "composer" => return '\u{E783}',        // dev-composer
            "lua" | "luajit" => return '\u{E826}',  // dev-lua
            "swift" | "swiftc" => return '\u{E755}', // dev-swift
            "zig" => return '\u{E8EF}',             // dev-zig
            "dotnet" => return '\u{E77F}',          // dev-dotnet
            "node" | "nodejs" => return '\u{E719}', // dev-nodejs
            "npm" | "npx" => return '\u{E71E}',     // dev-npm
            "pnpm" => return '\u{E865}',            // dev-pnpm
            "yarn" | "yarnpkg" => return '\u{E8EC}', // dev-yarn
            "deno" => return '\u{E7C0}',            // dev-denojs
            "bun" => return '\u{E76F}',             // dev-bun
            "docker" | "docker-compose" => return '\u{E7B0}', // dev-docker
            "podman" => return '\u{E866}',          // dev-podman
            "make" | "gmake" => return '\u{F1323}', // md-hammer-wrench
            "cmake" => return '\u{E794}',           // dev-cmake
            "ninja" => return '\u{F0774}',          // md-ninja
            "kubectl" | "k9s" | "minikube" => return '\u{E81D}', // dev-kubernetes
            "helm" => return '\u{E7FB}',            // dev-helm
            "terraform" | "tofu" | "opentofu" => return '\u{E8BD}', // dev-terraform
            "ansible" | "ansible-playbook" => return '\u{E723}', // dev-ansible
            "pulumi" => return '\u{E873}',          // dev-pulumi
            "aws" => return '\u{E7AD}',             // dev-aws
            "az" | "azure" => return '\u{E754}',    // dev-azure
            "gcloud" => return '\u{E7F1}',          // dev-googlecloud
            "cloudflared" | "wrangler" => return '\u{E792}', // dev-cloudflare
            "vercel" => return '\u{E8D3}',          // dev-vercel
            "netlify" => return '\u{E83C}',         // dev-netlify
            "psql" | "postgres" | "postmaster" => return '\u{E76E}', // dev-postgresql
            "mysql" | "mysqld" => return '\u{E704}', // dev-mysql
            "mariadb" | "mariadbd" => return '\u{E828}', // dev-mariadb
            "redis-cli" | "redis-server" | "redis-sentinel" => {
                return '\u{E76D}'; // dev-redis
            }
            "sqlite" | "sqlite3" => return '\u{E7C4}', // dev-sqlite
            "mongo" | "mongod" | "mongosh" => return '\u{E7A4}', // dev-mongodb
            _ => {}
        }
    }
    if has_cwd {
        '\u{F07B}' // fa-folder
    } else {
        // When: `has_cwd` is false, no process or directory is known, so the generic glyph shows.
        '\u{F489}' // oct-terminal — generic shell fallback
    }
}

/// Take the trailing two components of a cwd, separated by `/`. Trailing
/// slashes are stripped. A single-component path returns just that
/// component. The empty / root path returns `/`.
fn cwd_two_components(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        // When: `trimmed` is empty, the path was only slashes, so the root marker stands in.
        return "/".to_string();
    }
    let comps: Vec<&str> = trimmed.split('/').filter(|component| !component.is_empty()).collect();
    match comps.as_slice() {
        [] => "/".to_string(),
        [only] => (*only).to_string(),
        [.., parent, leaf] => format!("{parent}/{leaf}"),
    }
}

#[cfg(test)]
#[path = "tab_title_tests.rs"]
mod tab_title_tests;
