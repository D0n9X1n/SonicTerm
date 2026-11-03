//! Pins the command line: `--endpoint` takes a value, `--version` and `--help` stand alone, and
//! anything else is an error.

use super::*;

/// The arguments as owned strings.
fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

/// Each accepted form parses; a missing value or an unknown argument is refused.
#[test]
fn arguments_parse() {
    assert_eq!(parse_args(&args(&[])), Ok(Command::Relay(None)));
    assert_eq!(
        parse_args(&args(&["--endpoint", "/tmp/1.sock"])),
        Ok(Command::Relay(Some("/tmp/1.sock".into())))
    );
    assert_eq!(parse_args(&args(&["--version"])), Ok(Command::Version));
    assert_eq!(parse_args(&args(&["-h"])), Ok(Command::Help));
    assert!(parse_args(&args(&["--endpoint"])).is_err());
    assert!(parse_args(&args(&["--bogus"])).is_err());
}
