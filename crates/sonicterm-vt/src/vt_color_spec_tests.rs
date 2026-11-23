use super::*;

/// Every spec form Ghostty accepts parses to the same 8-bit color: `rgb:` with 1 to 4 hex digits
/// per channel (scaled, truncating), `rgbi:` intensities, `#` with 1 to 4 digits per channel, and
/// unprefixed 3 or 6 hex digits.
#[test]
fn every_spec_form_parses_to_its_color() {
    for (spec, expected) in [
        ("rgb:f/8/0", (255, 136, 0)),
        ("rgb:ff/88/00", (255, 136, 0)),
        ("rgb:fff/888/000", (255, 136, 0)),
        ("rgb:ffff/8888/0000", (255, 136, 0)),
        ("rgb:ff/800/0", (255, 127, 0)),
        ("rgbi:1/0.5/0", (255, 127, 0)),
        ("rgbi:+1/.5/-0", (255, 127, 0)),
        ("#f80", (255, 136, 0)),
        ("#ff8800", (255, 136, 0)),
        ("#fff888000", (255, 136, 0)),
        ("#ffff88880000", (255, 136, 0)),
        ("ff8800", (255, 136, 0)),
        ("f80", (255, 136, 0)),
        ("  #FF8800\t", (255, 136, 0)),
    ] {
        assert_eq!(parse_color_spec(spec.as_bytes()), Some(expected), "{spec}");
    }
}

/// X11 names match case-insensitively, with their inner spaces, and win over the unprefixed hex
/// reading of a three-letter name; a non-name falls through to hex.
#[test]
fn x11_names_match_case_insensitively_before_bare_hex() {
    assert_eq!(parse_color_spec(b"red"), Some((255, 0, 0)));
    assert_eq!(parse_color_spec(b"ForestGreen"), Some((34, 139, 34)));
    assert_eq!(parse_color_spec(b"FoReStGReen"), Some((34, 139, 34)));
    assert_eq!(parse_color_spec(b"medium spring green"), Some((0, 250, 154)));
    assert_eq!(parse_color_spec(b"ghost white"), Some((248, 248, 255)));
    assert_eq!(parse_color_spec(b"gray50"), Some((127, 127, 127)));
    assert_eq!(parse_color_spec(b"bad"), Some((187, 170, 221)), "not a name, so bare hex");
    assert_eq!(parse_color_spec(b"nosuchcolor"), None);
}

/// Malformed specs are refused whole, never read in part: wrong lengths, non-hex digits, a
/// missing channel, a sign or separator in a hex channel, an intensity outside 0..=1, a
/// wrong-case prefix and an empty spec.
#[test]
fn malformed_specs_are_refused() {
    for spec in [
        "",
        "   ",
        "#",
        "#ff",
        "#fffff",
        "#ggg",
        "ff88",
        "rgb:f/8",
        "rgb:f/8/",
        "rgb:/8/0",
        "rgb:fffff/8/0",
        "rgb:+f/8/0",
        "rgb:f_f/8/0",
        "rgb:f/8/0/1",
        "RGB:f/8/0",
        "rgb;f/8/0",
        "rgbi:1.5/0/0",
        "rgbi:-0.5/0/0",
        "rgbi:./0/0",
        "rgbi:1e0/0/0",
        "?",
    ] {
        assert_eq!(parse_color_spec(spec.as_bytes()), None, "{spec:?}");
    }
}

/// The embedded name table is sorted and lower case, which the binary search relies on, and
/// carries the whole X11 set.
#[test]
fn the_name_table_is_sorted_lower_case_and_complete() {
    assert!(X11_COLORS.len() > 700);
    assert!(X11_COLORS.windows(2).all(|pair| pair[0].0.as_bytes() < pair[1].0.as_bytes()));
    assert!(X11_COLORS.iter().all(|(name, _)| !name.bytes().any(|byte| byte.is_ascii_uppercase())));
}
