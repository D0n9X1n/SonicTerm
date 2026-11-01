// The bindgen-generated library root also compiles this file and carries no lint attributes, so it enables the naming lint.
#![warn(clippy::min_ident_chars)]

const REQUIRED_OPTIONS: [&str; 6] = [
    "FT_CONFIG_OPTION_ERROR_STRINGS",
    "FT_CONFIG_OPTION_SYSTEM_ZLIB",
    "FT_CONFIG_OPTION_USE_PNG",
    "PCF_CONFIG_OPTION_LONG_FAMILY_NAMES",
    "FT_CONFIG_OPTION_SUBPIXEL_RENDERING",
    "TT_CONFIG_OPTION_SUBPIXEL_HINTING",
];

/// The switch that turns FreeType's HVF driver from a rejecting stub into Apple's `hvgl` renderer.
const HVF_OPTION: &str = "FT_CONFIG_OPTION_HVF";

/// First macOS release whose system `libhvf` exports the renderer FreeType calls.
const HVF_RUNTIME_FLOOR: (u32, u32) = (15, 4);

/// Enables the supported boolean options, sets the HVF switch to `hvf`, and rejects ambiguous
/// upstream configuration.
///
/// The HVF switch must be declared exactly once, without a value. With `hvf` it is defined; without
/// it, it must already be commented out, so an upstream default that enables it cannot include
/// `hvf/Scaler.h` on a host that lacks it.
pub(crate) fn configure_freetype(source: &str, hvf: bool) -> Result<String, String> {
    let mut counts = [0; REQUIRED_OPTIONS.len()];
    let mut hvf_count = 0;
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
                        if name == HVF_OPTION {
                            // When: the HVF switch has a value, it is not the upstream boolean.
                            if tokens.next().is_some() {
                                return Err(format!(
                                    "FreeType option {name} must be a valueless boolean"
                                ));
                            }
                            hvf_count += 1;
                            let commented = candidate != trimmed;
                            if hvf {
                                replacement = Some(format!("#define {name}"));
                            } else if !commented {
                                // When: HVF was not requested yet upstream defines it, the build would need its SDK.
                                return Err(format!(
                                    "FreeType option {name} is enabled upstream but this build has no HVF SDK"
                                ));
                            }
                        } else if let Some(index) =
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
    if hvf_count != 1 {
        // When: the HVF switch is missing or repeated, its effective state is unverifiable.
        return Err(format!(
            "FreeType option {HVF_OPTION}: expected one declaration, found {hvf_count}"
        ));
    }
    Ok(configured)
}

/// Checks the generated options after the C preprocessor resolves upstream conditionals, including
/// that the HVF switch is defined exactly when `hvf` asked for it.
pub(crate) fn configuration_probe(hvf: bool) -> String {
    let mut probe = String::from("#include <freetype/config/ftoption.h>\n");
    for name in REQUIRED_OPTIONS {
        probe.push_str(&format!(
            "#ifndef {name}\n#error Required FreeType option {name} is disabled\n#endif\n"
        ));
    }
    let (directive, state) = if hvf { ("ifndef", "disabled") } else { ("ifdef", "enabled") };
    probe.push_str(&format!(
        "#{directive} {HVF_OPTION}\n#error FreeType option {HVF_OPTION} is unexpectedly {state}\n#endif\n"
    ));
    probe
}

/// The `(major, minor)` of a macOS version written `major[.minor[.patch]]`, or `None` when any
/// component is not a number.
// The standalone binding-config helper compiles this file without the build-script callers.
#[allow(dead_code)]
pub(crate) fn parse_macos_version(version: &str) -> Option<(u32, u32)> {
    let mut components = version.trim().split('.').map(|component| component.parse::<u32>().ok());
    let major = components.next()??;
    let minor = components.next().unwrap_or(Some(0))?;
    // Any further component must still be numeric, so `15.4.x` is unreadable rather than 15.4.
    components.all(|component| component.is_some()).then_some((major, minor))
}

/// Whether the Apple build compiles FreeType's HVF driver, or why it must stop.
///
/// `sdk_has_hvf` says the SDK ships `hvf/Scaler.h` and `libhvf.tbd`; `deployment_target` is
/// `MACOSX_DEPLOYMENT_TARGET`; `require_hvf` is `SONICTERM_REQUIRE_HVF=1`. HVF builds only below the
/// [`HVF_RUNTIME_FLOOR`]: there `__builtin_available` stays a runtime check and the `libhvf` load is
/// weak, while an unset target lets `cc` default to the SDK version and folds the check away.
// The standalone binding-config helper compiles this file without the build-script callers.
#[allow(dead_code)]
pub(crate) fn hvf_build_decision(
    sdk_has_hvf: bool,
    deployment_target: Option<&str>,
    require_hvf: bool,
) -> Result<bool, String> {
    if !sdk_has_hvf {
        // When: the SDK lacks the HVF header or library, only a required build is an error.
        return if require_hvf {
            Err("SONICTERM_REQUIRE_HVF=1 but the macOS SDK has no usr/include/hvf/Scaler.h and \
                 usr/lib/libhvf.tbd"
                .to_string())
        } else {
            Ok(false)
        };
    }
    match deployment_target.and_then(parse_macos_version) {
        Some(version) if version < HVF_RUNTIME_FLOOR => Ok(true),
        _ => Err(format!(
            "HVF needs MACOSX_DEPLOYMENT_TARGET below {}.{} so libhvf loads weakly; found {:?}",
            HVF_RUNTIME_FLOOR.0, HVF_RUNTIME_FLOOR.1, deployment_target
        )),
    }
}

#[cfg(test)]
#[path = "build_config_tests.rs"]
mod build_config_tests;
