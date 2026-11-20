use super::*;

/// Notices with different occasions keep a slot each, so a suppressed `unfocused` one never
/// hides a later `always` one; same-occasion notices beyond the first are only counted.
#[test]
fn each_occasion_keeps_its_own_first_notice() {
    let mut notices = ProgramNotices::default();
    notices.push_notice(None, "only when away".into(), None, NotifyOccasion::Unfocused);
    notices.push(None, "always".into());
    notices.push(None, "always again".into());
    let kept: Vec<_> =
        notices.into_notices().map(|notice| (notice.body, notice.occasion, notice.more)).collect();
    assert_eq!(
        kept,
        vec![
            ("always".into(), NotifyOccasion::Always, 1),
            ("only when away".into(), NotifyOccasion::Unfocused, 0)
        ]
    );
}

/// A later notice with the kept one's id replaces its text instead of counting, as kitty updates
/// a notification in place; a different id only counts.
#[test]
fn a_repeated_id_replaces_the_kept_notice() {
    let mut notices = ProgramNotices::default();
    let id = || Some("job".to_owned());
    notices.push_notice(Some("Build".into()), "running".into(), id(), NotifyOccasion::Always);
    notices.push_notice(Some("Build".into()), "done".into(), id(), NotifyOccasion::Always);
    notices.push_notice(None, "other".into(), Some("x".into()), NotifyOccasion::Always);
    let kept: Vec<_> = notices.into_notices().collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (kept[0].body.as_str(), kept[0].id.as_deref(), kept[0].more),
        ("done", Some("job"), 1)
    );
}
