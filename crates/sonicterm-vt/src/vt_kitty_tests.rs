//! Pins the kitty control-section parser and reply format: accepted keys and defaults, the
//! refusal of non-direct media and image numbers, continuation detection, and quiet modes.

use super::*;

#[test]
fn claude_codes_first_chunk_parses_to_a_virtual_png_transmit() {
    // The header Claude Code sends: PNG, transmit and display, quiet, virtual placement, more to come.
    let command = KittyCommand::parse(b"a=T,t=d,f=100,q=2,U=1,i=7,c=16,r=6,m=1").expect("valid");
    assert_eq!(command.action, KittyAction::TransmitAndDisplay);
    assert_eq!(command.medium, KittyMedium::Direct);
    assert_eq!((command.format, command.quiet, command.image_id), (100, 2, 7));
    assert_eq!((command.cols, command.rows), (16, 6));
    assert!(command.virtual_placement && command.more && !command.continuation_only);
}

#[test]
fn defaults_follow_the_protocol() {
    // An empty control section is a direct RGBA transmit with every reply enabled.
    let command = KittyCommand::parse(b"").expect("valid");
    assert_eq!(command.action, KittyAction::Transmit);
    assert_eq!((command.format, command.quiet, command.more), (32, 0, false));
    assert!(!command.continuation_only, "no keys is not a continuation");
}

#[test]
fn only_m_and_q_keys_make_a_continuation() {
    // A later chunk carries m (and possibly q); any other key starts a new command.
    assert!(KittyCommand::parse(b"m=1").expect("valid").continuation_only);
    assert!(KittyCommand::parse(b"m=0,q=2").expect("valid").continuation_only);
    assert!(!KittyCommand::parse(b"m=1,i=3").expect("valid").continuation_only);
}

#[test]
fn file_temporary_and_shared_memory_media_are_refused() {
    // A program may never make the terminal read a path or shm object it names.
    for medium in [&b"t=f"[..], b"t=t", b"t=s"] {
        assert_eq!(KittyCommand::parse(medium).expect("parsed").medium, KittyMedium::Refused);
    }
    assert_eq!(KittyCommand::parse(b"t=x"), Err(KittyCommandError::Invalid));
}

#[test]
fn image_numbers_compression_and_unknown_actions_are_rejected() {
    // I= is not an alias for i=; o= and actions outside t,T,q,p,d are not implemented.
    assert_eq!(KittyCommand::parse(b"a=T,I=4"), Err(KittyCommandError::Invalid));
    assert_eq!(KittyCommand::parse(b"o=z"), Err(KittyCommandError::Unsupported));
    assert_eq!(KittyCommand::parse(b"a=f"), Err(KittyCommandError::Unsupported));
    assert_eq!(KittyCommand::parse(b"f=8"), Err(KittyCommandError::Invalid));
}

#[test]
fn malformed_values_are_invalid_and_unknown_keys_are_ignored() {
    // Numbers must be decimal; a key without a value is malformed; unknown keys are skipped.
    assert_eq!(KittyCommand::parse(b"i=abc"), Err(KittyCommandError::Invalid));
    assert_eq!(KittyCommand::parse(b"i"), Err(KittyCommandError::Invalid));
    assert_eq!(KittyCommand::parse(b"i=99999999999"), Err(KittyCommandError::Invalid));
    assert_eq!(KittyCommand::parse(b"i=5,z=3,X=1").expect("valid").image_id, 5);
}

#[test]
fn delete_selectors_map_to_the_supported_subset() {
    // d=i and d=I are supported; every other selector parses but is unsupported.
    let by_id = KittyCommand::parse(b"a=d,d=i,i=2").expect("valid");
    assert_eq!((by_id.action, by_id.delete), (KittyAction::Delete, Some(KittyDelete::ById)));
    assert_eq!(
        KittyCommand::parse(b"a=d,d=I,i=2").expect("valid").delete,
        Some(KittyDelete::ByIdFree)
    );
    assert_eq!(
        KittyCommand::parse(b"a=d,d=a").expect("valid").delete,
        Some(KittyDelete::Unsupported)
    );
}

#[test]
fn replies_follow_kittys_format_and_quiet_rules() {
    // Replies name the image (and placement when set); q=1 drops OK, q=2 drops all, no id drops all.
    let loud = KittyCommand::parse(b"i=31,p=4").expect("valid");
    assert_eq!(loud.reply("OK").expect("sent"), b"\x1b_Gi=31,p=4;OK\x1b\\".to_vec());
    let errors_only = KittyCommand::parse(b"i=31,q=1").expect("valid");
    assert!(errors_only.reply("OK").is_none());
    assert_eq!(errors_only.reply("EINVAL:x").expect("sent"), b"\x1b_Gi=31;EINVAL:x\x1b\\".to_vec());
    assert!(KittyCommand::parse(b"i=31,q=2").expect("valid").reply("EINVAL:x").is_none());
    assert!(KittyCommand::parse(b"a=q").expect("valid").reply("OK").is_none());
}

#[test]
fn only_transmits_and_queries_carry_image_data() {
    // Place and delete have no payload to stage.
    assert!(KittyCommand::parse(b"a=q").expect("valid").carries_image());
    assert!(KittyCommand::parse(b"a=T").expect("valid").carries_image());
    assert!(!KittyCommand::parse(b"a=p,i=1").expect("valid").carries_image());
    assert!(!KittyCommand::parse(b"a=d").expect("valid").carries_image());
}

#[test]
fn flag_values_outside_their_domain_are_invalid() {
    // m and U are 0 or 1 and q is 0 to 2; any other value is a malformed command, not a default.
    for control in [&b"m=2"[..], b"q=3", b"U=2"] {
        assert_eq!(KittyCommand::parse(control), Err(KittyCommandError::Invalid), "{control:?}");
    }
}
