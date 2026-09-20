const REQUIRED_OPTIONS: [&str; 6] = [
    "FT_CONFIG_OPTION_ERROR_STRINGS",
    "FT_CONFIG_OPTION_SYSTEM_ZLIB",
    "FT_CONFIG_OPTION_USE_PNG",
    "PCF_CONFIG_OPTION_LONG_FAMILY_NAMES",
    "FT_CONFIG_OPTION_SUBPIXEL_RENDERING",
    "TT_CONFIG_OPTION_SUBPIXEL_HINTING",
];

/// Enables the supported boolean options and rejects ambiguous upstream configuration.
pub(crate) fn configure_freetype(source: &str) -> Result<String, String> {
    let mut counts = [0; REQUIRED_OPTIONS.len()];
    let mut configured = String::with_capacity(source.len());
    let mut in_comment = false;
    for line in source.lines() {
        let trimmed = line.trim();
        let candidate = trimmed
            .strip_prefix("/*")
            .and_then(|text| text.strip_suffix("*/"))
            .map(str::trim)
            .unwrap_or(trimmed);
        let mut replacement = None;
        // When: multiline documentation mentions macros, only declarations outside it establish options.
        if !in_comment {
            if let Some(directive) = candidate.strip_prefix('#') {
                let mut tokens = directive.split_whitespace();
                if tokens.next() == Some("define") {
                    if let Some(name) = tokens.next() {
                        if let Some(index) =
                            REQUIRED_OPTIONS.iter().position(|option| *option == name)
                        {
                            // When: an option gains a value, its semantics require review rather than silent coercion.
                            if tokens.next().is_some() {
                                return Err(format!(
                                    "FreeType option {name} must be a valueless boolean"
                                ));
                            }
                            counts[index] += 1;
                            replacement = Some(format!("#define {name}"));
                        }
                    }
                }
            }
        }
        configured.push_str(replacement.as_deref().unwrap_or(line));
        configured.push('\n');
        let mut rest = line;
        loop {
            let marker = if in_comment { "*/" } else { "/*" };
            let Some(index) = rest.find(marker) else { break };
            if !in_comment && rest[..index].contains("//") {
                break;
            }
            in_comment = !in_comment;
            rest = &rest[index + marker.len()..];
        }
    }
    for (name, count) in REQUIRED_OPTIONS.iter().zip(counts) {
        // When: missing or repeated definitions make the upstream option contract unverifiable, stop the build.
        if count != 1 {
            return Err(format!("FreeType option {name}: expected one declaration, found {count}"));
        }
    }
    Ok(configured)
}

/// Checks the generated options after the C preprocessor resolves upstream conditionals.
pub(crate) fn configuration_probe() -> String {
    let mut probe = String::from("#include <freetype/config/ftoption.h>\n");
    for name in REQUIRED_OPTIONS {
        probe.push_str(&format!(
            "#ifndef {name}\n#error Required FreeType option {name} is disabled\n#endif\n"
        ));
    }
    probe
}

#[cfg(test)]
#[path = "build_config_tests.rs"]
mod build_config_tests;
