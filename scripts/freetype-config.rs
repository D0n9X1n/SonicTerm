use std::{env, fs, path::PathBuf};

#[path = "../crates/sonicterm-freetype/build_config.rs"]
mod build_config;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let source = PathBuf::from(args.next().ok_or("missing upstream ftoption.h path")?);
    let destination = PathBuf::from(args.next().ok_or("missing output directory")?);
    let options = build_config::configure_freetype(&fs::read_to_string(source)?)?;
    fs::create_dir_all(destination.join("freetype/config"))?;
    fs::write(destination.join("freetype/config/ftoption.h"), options)?;
    fs::write(destination.join("config_probe.c"), build_config::configuration_probe())?;
    Ok(())
}
