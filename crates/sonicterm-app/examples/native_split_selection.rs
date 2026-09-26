#[cfg(target_os = "macos")]
#[path = "../tests/native_split_selection/mod.rs"]
mod native_split_selection;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let Some(flag) = args.next() else {
        println!("NOT_EXERCISED: pass --run and a new absolute scratch directory under the OS temp directory");
        return Ok(());
    };
    anyhow::ensure!(flag == "--run", "expected --run");
    let scratch = args.next().ok_or_else(|| anyhow::anyhow!("missing scratch directory"))?;
    anyhow::ensure!(args.next().is_none(), "unexpected extra argument");
    native_split_selection::run(std::path::Path::new(&scratch))
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("NOT_EXERCISED: this opt-in example requires macOS; Windows uses the native split-selection integration test");
}
