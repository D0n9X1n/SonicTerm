//! Stamp the build with the commit it was built from and that commit's date, for the About window.
//!
//! `SONICTERM_BUILD_COMMIT` is the short commit and `SONICTERM_BUILD_DATE` the commit's committer
//! date in UTC (`YYYY-MM-DD`), so the same commit stamps the same date on every platform and every
//! rebuild. `SOURCE_DATE_EPOCH`, when set, gives the date instead. A build outside a Git checkout
//! may set both variables itself; otherwise they are empty and the About window omits them.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "src/build_watch.rs"]
mod build_watch;

/// Seconds in one day.
const SECONDS_PER_DAY: i64 = 86_400;

fn main() {
    for variable in ["SONICTERM_BUILD_COMMIT", "SONICTERM_BUILD_DATE", "SOURCE_DATE_EPOCH"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    watch_head();
    let commit = std::env::var("SONICTERM_BUILD_COMMIT")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| git(&["rev-parse", "--short=8", "HEAD"]))
        .unwrap_or_default();
    let date = std::env::var("SONICTERM_BUILD_DATE")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let epoch = std::env::var("SOURCE_DATE_EPOCH")
                .ok()
                .or_else(|| git(&["show", "-s", "--format=%ct", "HEAD"]))?;
            epoch.trim().parse::<i64>().ok().map(utc_date)
        })
        .unwrap_or_default();
    println!("cargo:rustc-env=SONICTERM_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=SONICTERM_BUILD_DATE={date}");
}

/// Run `git` with `args` in the package directory; `None` when Git is missing or fails.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (output.status.success() && !text.is_empty()).then(|| text.to_owned())
}

/// Rebuild when HEAD moves: watch HEAD, the loose ref it names (even while the branch is packed,
/// so the commit that writes it is seen) and `packed-refs`, a worktree's refs in the common dir.
fn watch_head() {
    let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]).map(PathBuf::from) else {
        // When: git found no repository, there is nothing to watch.
        return;
    };
    let common_dir = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .map_or_else(|| git_dir.clone(), PathBuf::from);
    let head_ref = git(&["symbolic-ref", "-q", "HEAD"]);
    for path in
        build_watch::head_watch_paths(&git_dir, &common_dir, head_ref.as_deref(), Path::exists)
    {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// The UTC calendar date of Unix time `epoch_seconds`, as `YYYY-MM-DD`.
fn utc_date(epoch_seconds: i64) -> String {
    // Days since 1970-01-01 to a proleptic Gregorian date, counted in 400-year eras from March.
    let days = epoch_seconds.div_euclid(SECONDS_PER_DAY) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}
