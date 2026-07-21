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
//! The OSC 0/2 "raw" window title is used as a last-resort body when no cwd
//! is available — handy for commands like `ssh user@host` which set the
//! title themselves but never report a cwd.

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
    let n = index + 1;
    let icon = icon_for_process(process, cwd.is_some());

    let body = if let Some(c) = cwd {
        cwd_two_components(c)
    } else if let Some(t) = raw_title.map(str::trim).filter(|s| !s.is_empty()) {
        t.to_string()
    } else {
        "shell".to_string()
    };

    format!("#{n} {icon} {body}")
}

/// Pick the Nerd Font glyph for a process name. Returns the folder icon
/// when `has_cwd` is true and the process is unknown / absent. Returns a
/// terminal icon when neither is known.
fn icon_for_process(process: Option<&str>, has_cwd: bool) -> char {
    if let Some(p) = process {
        match p.to_ascii_lowercase().as_str() {
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
            "tmux" => return '\u{EBC8}',                  // cod-terminal-tmux
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
        '\u{F489}' // oct-terminal — generic shell fallback
    }
}

/// Take the trailing two components of a cwd, separated by `/`. Trailing
/// slashes are stripped. A single-component path returns just that
/// component. The empty / root path returns `/`.
fn cwd_two_components(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    let comps: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    match comps.as_slice() {
        [] => "/".to_string(),
        [only] => (*only).to_string(),
        [.., parent, leaf] => format!("{parent}/{leaf}"),
    }
}

#[cfg(test)]
#[path = "tab_title_tests.rs"]
mod tab_title_tests;
