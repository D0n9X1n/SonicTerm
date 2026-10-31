#![warn(clippy::min_ident_chars)]

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

mod build_config;

fn new_build() -> cc::Build {
    let mut cfg = cc::Build::new();
    cfg.warnings(false);
    cfg.flag_if_supported("-fno-stack-check");
    cfg.flag_if_supported("-Wno-deprecated-non-prototype");
    cfg
}

fn zlib() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let mut cfg = new_build();
    let build_dir = out_dir.join("zlib-build");
    std::fs::create_dir_all(&build_dir).unwrap();
    cfg.out_dir(&build_dir);
    cfg.file("zlib/adler32.c")
        .file("zlib/compress.c")
        .file("zlib/crc32.c")
        .file("zlib/deflate.c")
        .file("zlib/gzclose.c")
        .file("zlib/gzlib.c")
        .file("zlib/gzread.c")
        .file("zlib/gzwrite.c")
        .file("zlib/inflate.c")
        .file("zlib/infback.c")
        .file("zlib/inftrees.c")
        .file("zlib/inffast.c")
        .file("zlib/trees.c")
        .file("zlib/uncompr.c")
        .file("zlib/zutil.c");
    cfg.include("zlib");
    cfg.define("HAVE_SYS_TYPES_H", None);
    cfg.define("HAVE_STDINT_H", None);
    cfg.define("HAVE_STDDEF_H", None);
    let target = env::var("TARGET").unwrap();
    if !target.contains("windows") {
        cfg.define("_LARGEFILE64_SOURCE", Some("1"));
    }
    cfg.compile("z");
}

fn libpng() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let mut cfg = new_build();
    let build_dir = out_dir.join("png-build");
    std::fs::create_dir_all(&build_dir).unwrap();
    cfg.out_dir(&build_dir);

    cfg.file("libpng/png.c")
        .file("libpng/pngerror.c")
        .file("libpng/pngget.c")
        .file("libpng/pngmem.c")
        .file("libpng/pngpread.c")
        .file("libpng/pngread.c")
        .file("libpng/pngrio.c")
        .file("libpng/pngrtran.c")
        .file("libpng/pngrutil.c")
        .file("libpng/pngset.c")
        .file("libpng/pngtrans.c")
        .file("libpng/pngwio.c")
        .file("libpng/pngwrite.c")
        .file("libpng/pngwtran.c")
        .file("libpng/pngwutil.c");

    if let Ok(arch) = env::var("CARGO_CFG_TARGET_ARCH") {
        match arch.as_str() {
            "aarch64" | "arm" => {
                cfg.file("libpng/arm/arm_init.c")
                    .file("libpng/arm/filter_neon.S")
                    .file("libpng/arm/filter_neon_intrinsics.c")
                    .file("libpng/arm/palette_neon_intrinsics.c");
            }
            _ => {}
        }
    }

    cfg.include("zlib");
    cfg.include("libpng");
    cfg.include(&build_dir);
    cfg.define("HAVE_SYS_TYPES_H", None);
    cfg.define("HAVE_STDINT_H", None);
    cfg.define("HAVE_STDDEF_H", None);
    let target = env::var("TARGET").unwrap();
    if target.contains("powerpc64") {
        cfg.define("PNG_POWERPC_VSX_OPT", Some("0"));
    }
    if !target.contains("windows") {
        cfg.define("_LARGEFILE64_SOURCE", Some("1"));
    }

    std::fs::write(
        build_dir.join("pnglibconf.h"),
        std::fs::read_to_string("libpng/scripts/pnglibconf.h.prebuilt").unwrap(),
    )
    .unwrap();

    cfg.compile("png");
}

/// The macOS SDK the C build compiles against: `SDKROOT` when set, else `xcrun`'s default SDK.
fn apple_sdk_root() -> Option<PathBuf> {
    if let Some(root) = env::var_os("SDKROOT").filter(|root| !root.is_empty()) {
        // When: `SDKROOT` names an SDK, it decides for cc, rustc and the linker alike.
        return Some(PathBuf::from(root));
    }
    let output =
        Command::new("xcrun").args(["--sdk", "macosx", "--show-sdk-path"]).output().ok()?;
    if !output.status.success() {
        // When: `xcrun` fails there is no SDK to probe; the HVF decision treats it as absent.
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Whether `sdk` ships both the HVF header FreeType includes and the stub of the library it links.
fn sdk_has_hvf(sdk: &Path) -> bool {
    sdk.join("usr/include/hvf/Scaler.h").is_file() && sdk.join("usr/lib/libhvf.tbd").is_file()
}

/// Configures `cfg` for FreeType's HVF driver on macOS and reports whether it is enabled.
///
/// The driver calls the system `libhvf` only behind `__builtin_available(macOS 15.4, *)`, and the
/// deployment target stays below 15.4, so the library is linked as a weak dylib and an older macOS
/// still launches. Linux and Windows targets never enable it.
fn configure_hvf(cfg: &mut cc::Build) -> bool {
    for name in ["SDKROOT", "DEVELOPER_DIR", "MACOSX_DEPLOYMENT_TARGET", "SONICTERM_REQUIRE_HVF"] {
        println!("cargo:rerun-if-env-changed={}", name);
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        // When: the target is not macOS, there is no `libhvf` to render with.
        return false;
    }
    let sdk = apple_sdk_root();
    for watched in ["SDKSettings.json", "usr/include/hvf/Scaler.h"] {
        // A missing watched path would rerun the build script every time, so only existing ones.
        if let Some(path) = sdk.as_ref().map(|sdk| sdk.join(watched)).filter(|path| path.exists()) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    let require_hvf = env::var("SONICTERM_REQUIRE_HVF").as_deref() == Ok("1");
    let deployment_target = env::var("MACOSX_DEPLOYMENT_TARGET").ok();
    let has_hvf = sdk.as_deref().is_some_and(sdk_has_hvf);
    let hvf = build_config::hvf_build_decision(has_hvf, deployment_target.as_deref(), require_hvf)
        .unwrap_or_else(|error| panic!("FreeType HVF configuration: {}", error));
    let Some(sdk) = sdk.filter(|_| hvf) else {
        // When: the SDK lacks HVF and it is not required, keep upstream's stub driver.
        println!(
            "cargo:warning=macOS SDK has no hvf/Scaler.h; FreeType cannot open hvgl fonts such as PingFang"
        );
        return false;
    };
    // One SDK supplies both the header and the library stub; cc honours this over its own lookup.
    cfg.env("SDKROOT", &sdk);
    cfg.define("HVF_RUNTIME_AVAILABLE", Some("1"));
    // A link-lib, unlike a link-arg, reaches every binary that links this crate.
    println!("cargo:rustc-link-lib=dylib=hvf");
    true
}

fn freetype() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let mut cfg = new_build();
    let build_dir = out_dir.join("freetype-build");
    std::fs::create_dir_all(&build_dir).unwrap();
    cfg.out_dir(&build_dir);
    cfg.include("zlib");
    cfg.include("libpng");
    cfg.include(out_dir.join("png-build"));

    std::fs::create_dir_all(build_dir.join("freetype2/include/freetype/config")).unwrap();
    cfg.include(format!("{}/freetype2/include", build_dir.display()));
    cfg.include("freetype2/include");
    cfg.define("FT2_BUILD_LIBRARY", None);

    let target = env::var("TARGET").unwrap();
    let hvf = configure_hvf(&mut cfg);

    let upstream_options =
        std::fs::read_to_string("freetype2/include/freetype/config/ftoption.h").unwrap();
    let options = build_config::configure_freetype(&upstream_options, hvf)
        .unwrap_or_else(|error| panic!("invalid FreeType configuration: {}", error));
    std::fs::write(build_dir.join("freetype2/include/freetype/config/ftoption.h"), options)
        .unwrap();
    let probe = build_dir.join("config_probe.c");
    std::fs::write(&probe, build_config::configuration_probe(hvf)).unwrap();
    cfg.file(probe);

    for source in [
        "autofit/autofit.c",
        "base/ftbase.c",
        "base/ftbbox.c",
        "base/ftbdf.c",
        "base/ftbitmap.c",
        "base/ftcid.c",
        "base/ftfstype.c",
        "base/ftgasp.c",
        "base/ftglyph.c",
        "base/ftgxval.c",
        "base/ftinit.c",
        "base/ftmm.c",
        "base/ftotval.c",
        "base/ftpatent.c",
        "base/ftpfr.c",
        "base/ftstroke.c",
        "base/ftsynth.c",
        "base/ftsystem.c",
        "base/fttype1.c",
        "base/ftwinfnt.c",
        "bdf/bdf.c",
        "bzip2/ftbzip2.c",
        "cache/ftcache.c",
        "cff/cff.c",
        "cid/type1cid.c",
        "gzip/ftgzip.c",
        // ftmodule.h registers the HVF driver; without FT_CONFIG_OPTION_HVF it is a stub that rejects every face.
        "hvf/hvf.c",
        "lzw/ftlzw.c",
        "pcf/pcf.c",
        "pfr/pfr.c",
        "psaux/psaux.c",
        "pshinter/pshinter.c",
        "psnames/psnames.c",
        "raster/raster.c",
        "sdf/ftbsdf.c",
        "sdf/ftsdf.c",
        "sdf/ftsdfcommon.c",
        "sdf/ftsdfrend.c",
        "sfnt/sfnt.c",
        "smooth/smooth.c",
        "svg/ftsvg.c",
        "truetype/truetype.c",
        "type1/type1.c",
        "type42/type42.c",
        "winfonts/winfnt.c",
    ]
    .iter()
    {
        cfg.file(format!("freetype2/src/{}", source));
    }

    if target.contains("windows") {
        cfg.file("freetype2/builds/windows/ftdebug.c");
    } else {
        cfg.file("freetype2/src/base/ftdebug.c");
    }

    cfg.compile("freetype");

    // These cause DEP_FREETYPE_INCLUDE and DEP_FREETYPE_LIB to be
    // defined in the harfbuzz/build.rs
    println!(
        "cargo:include={}/freetype2/include;{}/freetype2/include",
        build_dir.display(),
        std::env::current_dir().unwrap().display()
    );
    println!("cargo:lib={}", build_dir.display());
}

fn main() {
    for source in ["build.rs", "build_config.rs", "freetype2", "libpng", "zlib"] {
        println!("cargo:rerun-if-changed={}", source);
    }
    zlib();
    libpng();
    freetype();
    let out_dir = env::var("OUT_DIR").unwrap();
    println!("cargo:outdir={}", out_dir);
}
