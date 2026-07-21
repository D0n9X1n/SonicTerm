use super::*;

struct ProcessFamily {
    category: &'static str,
    application: &'static str,
    aliases: &'static [&'static str],
    icon: char,
    glyph: &'static str,
}

const PROCESS_FAMILIES: &[ProcessFamily] = &[
    ProcessFamily {
        category: "AI",
        application: "Claude Code",
        aliases: &["claude", "claude-code"],
        icon: '\u{f0674}',
        glyph: "md-creation",
    },
    ProcessFamily {
        category: "AI",
        application: "GitHub Copilot CLI",
        aliases: &["copilot", "github-copilot", "github-copilot-cli"],
        icon: '\u{f4b8}',
        glyph: "oct-copilot",
    },
    ProcessFamily {
        category: "Shell",
        application: "Zsh",
        aliases: &["zsh"],
        icon: '\u{e84f}',
        glyph: "dev-ohmyzsh",
    },
    ProcessFamily {
        category: "Shell",
        application: "Bash",
        aliases: &["bash"],
        icon: '\u{e760}',
        glyph: "dev-bash",
    },
    ProcessFamily {
        category: "Shell",
        application: "Fish",
        aliases: &["fish"],
        icon: '\u{ee41}',
        glyph: "fa-fish",
    },
    ProcessFamily {
        category: "Shell",
        application: "POSIX shell",
        aliases: &["sh", "dash"],
        icon: '\u{e691}',
        glyph: "seti-shell",
    },
    ProcessFamily {
        category: "Shell",
        application: "PowerShell",
        aliases: &["pwsh", "powershell"],
        icon: '\u{ebc7}',
        glyph: "cod-terminal-powershell",
    },
    ProcessFamily {
        category: "Shell",
        application: "Command Prompt",
        aliases: &["cmd"],
        icon: '\u{ebc4}',
        glyph: "cod-terminal-cmd",
    },
    ProcessFamily {
        category: "Editor",
        application: "Vim / Neovim",
        aliases: &["nvim", "vim", "vi", "nvi"],
        icon: '\u{e62b}',
        glyph: "custom-vim",
    },
    ProcessFamily {
        category: "Editor",
        application: "Visual Studio Code",
        aliases: &["code", "code-insiders", "codium", "vscodium"],
        icon: '\u{e8da}',
        glyph: "dev-vscode",
    },
    ProcessFamily {
        category: "Editor",
        application: "Emacs",
        aliases: &["emacs", "emacsclient"],
        icon: '\u{e7cf}',
        glyph: "dev-emacs",
    },
    ProcessFamily {
        category: "Editor",
        application: "Nano",
        aliases: &["nano"],
        icon: '\u{e838}',
        glyph: "dev-nano",
    },
    ProcessFamily {
        category: "Remote / mux",
        application: "SSH / Mosh",
        aliases: &["ssh", "mosh"],
        icon: '\u{f08c0}',
        glyph: "md-ssh",
    },
    ProcessFamily {
        category: "Remote / mux",
        application: "tmux",
        aliases: &["tmux"],
        icon: '\u{ebc8}',
        glyph: "cod-terminal-tmux",
    },
    ProcessFamily {
        category: "Remote / mux",
        application: "GNU Screen",
        aliases: &["screen"],
        icon: '\u{eb4c}',
        glyph: "cod-screen-full",
    },
    ProcessFamily {
        category: "Source control",
        application: "Git",
        aliases: &["git", "lazygit", "tig"],
        icon: '\u{f1d3}',
        glyph: "fa-git",
    },
    ProcessFamily {
        category: "Source control",
        application: "GitHub CLI",
        aliases: &["gh", "hub"],
        icon: '\u{f470}',
        glyph: "oct-logo-github",
    },
    ProcessFamily {
        category: "Source control",
        application: "GitLab CLI",
        aliases: &["glab"],
        icon: '\u{e7eb}',
        glyph: "dev-gitlab",
    },
    ProcessFamily {
        category: "Language / build",
        application: "Rust",
        aliases: &["cargo", "rustc", "rust-analyzer"],
        icon: '\u{f1617}',
        glyph: "md-language-rust",
    },
    ProcessFamily {
        category: "Language / build",
        application: "Python",
        aliases: &["python", "python3", "ipython", "pip", "pip3"],
        icon: '\u{f0320}',
        glyph: "md-language-python",
    },
    ProcessFamily {
        category: "Language / build",
        application: "Go",
        aliases: &["go", "gofmt", "gopls"],
        icon: '\u{e724}',
        glyph: "dev-go",
    },
    ProcessFamily {
        category: "Language / build",
        application: "Java",
        aliases: &["java", "javac"],
        icon: '\u{e738}',
        glyph: "dev-java",
    },
    ProcessFamily {
        category: "Language / build",
        application: "Maven",
        aliases: &["mvn", "mvnw"],
        icon: '\u{e82c}',
        glyph: "dev-maven",
    },
    ProcessFamily {
        category: "Language / build",
        application: "Gradle",
        aliases: &["gradle", "gradlew"],
        icon: '\u{e7f2}',
        glyph: "dev-gradle",
    },
    ProcessFamily {
        category: "Language / build",
        application: "Ruby",
        aliases: &["ruby", "irb", "bundle", "bundler", "gem", "rails"],
        icon: '\u{e739}',
        glyph: "dev-ruby",
    },
    ProcessFamily {
        category: "Language / build",
        application: "PHP",
        aliases: &["php", "php-fpm"],
        icon: '\u{e73d}',
        glyph: "dev-php",
    },
    ProcessFamily {
        category: "Language / build",
        application: "Composer",
        aliases: &["composer"],
        icon: '\u{e783}',
        glyph: "dev-composer",
    },
    ProcessFamily {
        category: "Language / build",
        application: "Lua",
        aliases: &["lua", "luajit"],
        icon: '\u{e826}',
        glyph: "dev-lua",
    },
    ProcessFamily {
        category: "Language / build",
        application: "Swift",
        aliases: &["swift", "swiftc"],
        icon: '\u{e755}',
        glyph: "dev-swift",
    },
    ProcessFamily {
        category: "Language / build",
        application: "Zig",
        aliases: &["zig"],
        icon: '\u{e8ef}',
        glyph: "dev-zig",
    },
    ProcessFamily {
        category: "Language / build",
        application: ".NET",
        aliases: &["dotnet"],
        icon: '\u{e77f}',
        glyph: "dev-dotnet",
    },
    ProcessFamily {
        category: "Package / runtime",
        application: "Node.js",
        aliases: &["node", "nodejs"],
        icon: '\u{e719}',
        glyph: "dev-nodejs",
    },
    ProcessFamily {
        category: "Package / runtime",
        application: "npm",
        aliases: &["npm", "npx"],
        icon: '\u{e71e}',
        glyph: "dev-npm",
    },
    ProcessFamily {
        category: "Package / runtime",
        application: "pnpm",
        aliases: &["pnpm"],
        icon: '\u{e865}',
        glyph: "dev-pnpm",
    },
    ProcessFamily {
        category: "Package / runtime",
        application: "Yarn",
        aliases: &["yarn", "yarnpkg"],
        icon: '\u{e8ec}',
        glyph: "dev-yarn",
    },
    ProcessFamily {
        category: "Package / runtime",
        application: "Deno",
        aliases: &["deno"],
        icon: '\u{e7c0}',
        glyph: "dev-denojs",
    },
    ProcessFamily {
        category: "Package / runtime",
        application: "Bun",
        aliases: &["bun"],
        icon: '\u{e76f}',
        glyph: "dev-bun",
    },
    ProcessFamily {
        category: "Container / build",
        application: "Docker",
        aliases: &["docker", "docker-compose"],
        icon: '\u{e7b0}',
        glyph: "dev-docker",
    },
    ProcessFamily {
        category: "Container / build",
        application: "Podman",
        aliases: &["podman"],
        icon: '\u{e866}',
        glyph: "dev-podman",
    },
    ProcessFamily {
        category: "Container / build",
        application: "Make",
        aliases: &["make", "gmake"],
        icon: '\u{f1323}',
        glyph: "md-hammer-wrench",
    },
    ProcessFamily {
        category: "Container / build",
        application: "CMake",
        aliases: &["cmake"],
        icon: '\u{e794}',
        glyph: "dev-cmake",
    },
    ProcessFamily {
        category: "Container / build",
        application: "Ninja",
        aliases: &["ninja"],
        icon: '\u{f0774}',
        glyph: "md-ninja",
    },
    ProcessFamily {
        category: "DevOps / cloud",
        application: "Kubernetes",
        aliases: &["kubectl", "k9s", "minikube"],
        icon: '\u{e81d}',
        glyph: "dev-kubernetes",
    },
    ProcessFamily {
        category: "DevOps / cloud",
        application: "Helm",
        aliases: &["helm"],
        icon: '\u{e7fb}',
        glyph: "dev-helm",
    },
    ProcessFamily {
        category: "DevOps / cloud",
        application: "Terraform / OpenTofu",
        aliases: &["terraform", "tofu", "opentofu"],
        icon: '\u{e8bd}',
        glyph: "dev-terraform",
    },
    ProcessFamily {
        category: "DevOps / cloud",
        application: "Ansible",
        aliases: &["ansible", "ansible-playbook"],
        icon: '\u{e723}',
        glyph: "dev-ansible",
    },
    ProcessFamily {
        category: "DevOps / cloud",
        application: "Pulumi",
        aliases: &["pulumi"],
        icon: '\u{e873}',
        glyph: "dev-pulumi",
    },
    ProcessFamily {
        category: "DevOps / cloud",
        application: "AWS CLI",
        aliases: &["aws"],
        icon: '\u{e7ad}',
        glyph: "dev-aws",
    },
    ProcessFamily {
        category: "DevOps / cloud",
        application: "Azure CLI",
        aliases: &["az", "azure"],
        icon: '\u{e754}',
        glyph: "dev-azure",
    },
    ProcessFamily {
        category: "DevOps / cloud",
        application: "Google Cloud CLI",
        aliases: &["gcloud"],
        icon: '\u{e7f1}',
        glyph: "dev-googlecloud",
    },
    ProcessFamily {
        category: "DevOps / cloud",
        application: "Cloudflare",
        aliases: &["cloudflared", "wrangler"],
        icon: '\u{e792}',
        glyph: "dev-cloudflare",
    },
    ProcessFamily {
        category: "DevOps / cloud",
        application: "Vercel",
        aliases: &["vercel"],
        icon: '\u{e8d3}',
        glyph: "dev-vercel",
    },
    ProcessFamily {
        category: "DevOps / cloud",
        application: "Netlify",
        aliases: &["netlify"],
        icon: '\u{e83c}',
        glyph: "dev-netlify",
    },
    ProcessFamily {
        category: "Database",
        application: "PostgreSQL",
        aliases: &["psql", "postgres", "postmaster"],
        icon: '\u{e76e}',
        glyph: "dev-postgresql",
    },
    ProcessFamily {
        category: "Database",
        application: "MySQL",
        aliases: &["mysql", "mysqld"],
        icon: '\u{e704}',
        glyph: "dev-mysql",
    },
    ProcessFamily {
        category: "Database",
        application: "MariaDB",
        aliases: &["mariadb", "mariadbd"],
        icon: '\u{e828}',
        glyph: "dev-mariadb",
    },
    ProcessFamily {
        category: "Database",
        application: "Redis",
        aliases: &["redis-cli", "redis-server", "redis-sentinel"],
        icon: '\u{e76d}',
        glyph: "dev-redis",
    },
    ProcessFamily {
        category: "Database",
        application: "SQLite",
        aliases: &["sqlite", "sqlite3"],
        icon: '\u{e7c4}',
        glyph: "dev-sqlite",
    },
    ProcessFamily {
        category: "Database",
        application: "MongoDB",
        aliases: &["mongo", "mongod", "mongosh"],
        icon: '\u{e7a4}',
        glyph: "dev-mongodb",
    },
];

fn mixed_case(alias: &str) -> String {
    alias
        .chars()
        .enumerate()
        .map(|(index, ch)| if index % 2 == 0 { ch.to_ascii_uppercase() } else { ch })
        .collect()
}

#[test]
fn display_labels_prefix_only_tabs_after_the_first() {
    assert_eq!(tab_display_label(0, "first"), "first");
    assert_eq!(tab_display_label(1, "second"), "│ second");
    assert_eq!(tab_display_label(12, "later"), "│ later");
}

#[test]
fn cwd_takes_precedence_and_is_reduced_to_two_components() {
    let title =
        format_tab_title(2, Some("/Users/alice/project/"), Some("NVIM"), Some("ignored raw title"));

    assert_eq!(title, "#3 \u{e62b} alice/project");
}

#[test]
fn cwd_reduction_handles_root_single_and_repeated_separators() {
    assert_eq!(cwd_two_components("/"), "/");
    assert_eq!(cwd_two_components("////"), "/");
    assert_eq!(cwd_two_components("/tmp/"), "tmp");
    assert_eq!(cwd_two_components("//srv///repo//src///"), "repo/src");
    assert_eq!(cwd_two_components("relative/path"), "relative/path");
}

#[test]
fn raw_titles_are_trimmed_and_blank_titles_fall_back_to_shell() {
    assert_eq!(format_tab_title(0, None, None, Some("  build log  ")), "#1 \u{f489} build log");
    assert_eq!(format_tab_title(0, None, None, Some(" \t\n ")), "#1 \u{f489} shell");
    assert_eq!(format_tab_title(0, None, None, None), "#1 \u{f489} shell");
}

#[test]
fn unknown_processes_use_context_sensitive_fallback_icons() {
    assert_eq!(
        format_tab_title(0, Some("/work/tree"), Some("tool"), None),
        "#1 \u{f07b} work/tree"
    );
    assert_eq!(format_tab_title(0, None, Some("tool"), Some("remote")), "#1 \u{f489} remote");
}

#[test]
fn every_approved_alias_maps_case_insensitively_to_its_bundled_glyph() {
    for family in PROCESS_FAMILIES {
        for alias in family.aliases {
            for process in [(*alias).to_string(), mixed_case(alias)] {
                assert_eq!(
                    icon_for_process(Some(&process), false),
                    family.icon,
                    "{}/{} alias {process} should use {}",
                    family.category,
                    family.application,
                    family.glyph
                );
            }
        }
    }
}

#[test]
fn claude_and_copilot_are_distinct_non_generic_icons() {
    let claude = icon_for_process(Some("claude"), false);
    let copilot = icon_for_process(Some("copilot"), false);

    assert_eq!(claude, '\u{f0674}');
    assert_eq!(copilot, '\u{f4b8}');
    assert_ne!(claude, copilot);
    assert!(!['\u{f07b}', '\u{f489}'].contains(&claude));
    assert!(!['\u{f07b}', '\u{f489}'].contains(&copilot));
}

#[test]
fn corrected_families_do_not_use_stale_bundled_glyphs() {
    for alias in ["node", "npm", "npx", "pnpm", "yarn", "deno", "bun"] {
        assert_ne!(icon_for_process(Some(alias), false), '\u{f1842}', "alias {alias}");
    }
    for alias in ["docker", "docker-compose", "podman"] {
        assert_ne!(icon_for_process(Some(alias), false), '\u{f0867}', "alias {alias}");
    }
    for alias in ["make", "gmake", "cmake", "ninja"] {
        assert_ne!(icon_for_process(Some(alias), false), '\u{f05b4}', "alias {alias}");
    }
}

#[test]
fn process_matching_is_exact_and_does_not_parse_paths_arguments_or_titles() {
    assert_eq!(icon_for_process(Some("/usr/bin/node"), false), '\u{f489}');
    assert_eq!(icon_for_process(Some("node --version"), false), '\u{f489}');
    assert_eq!(icon_for_process(Some("node.exe"), false), '\u{f489}');
    assert_eq!(format_tab_title(0, None, Some("unknown"), Some("node")), "#1 \u{f489} node");
}
