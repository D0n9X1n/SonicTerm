//! Catalog priority, protection, and sensitivity contracts.

use super::*;

#[test]
fn catalog_rows_match_the_published_priority_table() {
    // The catalog table in the issue is the contract segment issues cite; every id has a row.
    use StatusSegmentId as Seg;
    let expected: [(Seg, &str, SegmentGroup, StatusSide, u16); 21] = [
        (Seg::Alerts, "alerts", SegmentGroup::OtherTabs, StatusSide::Right, 990),
        (Seg::Health, "health", SegmentGroup::Core, StatusSide::Right, 950),
        (Seg::Mem, "mem", SegmentGroup::Core, StatusSide::Right, 900),
        (Seg::Mode, "mode", SegmentGroup::Core, StatusSide::Left, 800),
        (Seg::Input, "input", SegmentGroup::Input, StatusSide::Left, 780),
        (Seg::Clip, "clip", SegmentGroup::ProgramReported, StatusSide::Right, 760),
        (Seg::PathCheck, "path_check", SegmentGroup::Diagnostics, StatusSide::Right, 700),
        (Seg::Link, "link", SegmentGroup::Pointer, StatusSide::Center, 600),
        (Seg::Paste, "paste", SegmentGroup::Input, StatusSide::Left, 560),
        (Seg::Sel, "sel", SegmentGroup::Pointer, StatusSide::Center, 520),
        (Seg::Ime, "ime", SegmentGroup::Input, StatusSide::Left, 500),
        (Seg::Scroll, "scroll", SegmentGroup::Core, StatusSide::Left, 400),
        (Seg::Cpu, "cpu", SegmentGroup::Diagnostics, StatusSide::Right, 380),
        (Seg::Frame, "frame", SegmentGroup::Diagnostics, StatusSide::Right, 360),
        (Seg::Out, "out", SegmentGroup::Diagnostics, StatusSide::Right, 340),
        (Seg::TabMem, "tab_mem", SegmentGroup::Diagnostics, StatusSide::Right, 330),
        (Seg::Glyphs, "glyphs", SegmentGroup::Display, StatusSide::Right, 320),
        (Seg::History, "history", SegmentGroup::Display, StatusSide::Right, 300),
        (Seg::Font, "font", SegmentGroup::Display, StatusSide::Right, 280),
        (Seg::Zoom, "zoom", SegmentGroup::Display, StatusSide::Left, 260),
        (Seg::Size, "size", SegmentGroup::Display, StatusSide::Right, 80),
    ];
    assert_eq!(StatusSegmentId::ALL.len(), expected.len());
    for (index, (id, name, group, side, priority)) in expected.into_iter().enumerate() {
        assert_eq!(StatusSegmentId::ALL[index], id, "catalog order");
        let meta = id.meta();
        assert_eq!(meta.id, id);
        assert_eq!(id.as_str(), name);
        assert_eq!(StatusSegmentId::from_name(name), Some(id));
        assert_eq!(serde_plain::to_string(&id).unwrap(), name, "serde spelling");
        assert_eq!(
            (meta.group, meta.home_side, meta.default_priority),
            (group, side, priority),
            "{name}"
        );
        assert!(meta.spoken_name_key.starts_with("status-spoken-"), "{name}");
    }
}

#[test]
fn protection_and_sensitivity_follow_the_catalog() {
    // Alerts, health errors, the broadcast chip and the link host survive narrowing longest.
    use StatusSegmentId as Seg;
    assert_eq!(Seg::Alerts.meta().protection, Protection::Always);
    assert_eq!(Seg::Health.meta().protection, Protection::Errors);
    assert_eq!(Seg::Mode.meta().protection, Protection::BroadcastChip);
    assert_eq!(Seg::Link.meta().protection, Protection::DestinationHost);
    assert_eq!(Seg::Link.meta().sensitivity, Sensitivity::Whole);
    assert!(!Seg::Link.meta().allows_always, "link has no idle text");
    assert_eq!(Seg::Mem.meta().default_show, ShowMode::Always);
    let implemented: Vec<Seg> = Seg::ALL.into_iter().filter(|id| id.meta().implemented).collect();
    assert_eq!(
        implemented,
        [
            Seg::Alerts,
            Seg::Health,
            Seg::Mem,
            Seg::Mode,
            Seg::Input,
            Seg::Clip,
            Seg::PathCheck,
            Seg::Link,
            Seg::Paste,
            Seg::Sel,
            Seg::Ime,
            Seg::Scroll,
            Seg::Cpu,
            Seg::Frame,
            Seg::Out,
            Seg::TabMem,
            Seg::Glyphs,
            Seg::History,
            Seg::Font,
            Seg::Zoom,
            Seg::Size
        ]
    );
    for id in [Seg::Paste, Seg::Ime] {
        assert!(!id.meta().allows_always && !id.meta().clickable, "{id:?} only explains input");
        assert_eq!(id.meta().sensitivity, Sensitivity::None, "{id:?} shows no typed text");
    }
}
