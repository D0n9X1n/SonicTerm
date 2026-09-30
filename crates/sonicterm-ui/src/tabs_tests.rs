use super::*;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

#[test]
fn empty_rename_reverts_to_auto_title() {
    let mut bar = TabBar::new();
    bar.push(Tab::new("#1 ~/work"));
    bar.set_active_custom_title("my work");
    assert_eq!(bar.tabs[0].custom_title.as_deref(), Some("my work"));
    assert_eq!(bar.tabs[0].title, "#1 my work");

    bar.set_active_custom_title("");
    assert_eq!(bar.tabs[0].custom_title, None);
    assert_eq!(bar.tabs[0].title, "#1 ~/work");

    bar.set_active_custom_title("renamed");
    bar.set_active_title("#1 ~/new-auto");
    assert_eq!(bar.tabs[0].title, "#1 renamed");
    bar.set_active_custom_title("   ");
    assert_eq!(bar.tabs[0].custom_title, None);
    assert_eq!(bar.tabs[0].title, "#1 ~/new-auto");
}

#[test]
fn active_custom_color_is_stored_on_active_tab() {
    let mut bar = TabBar::new();
    bar.push(Tab::new("#1 ~/work"));

    bar.set_active_custom_color("#fabd2f");

    assert_eq!(bar.active_custom_color(), Some("#fabd2f"));
    assert_eq!(bar.tabs[0].custom_color.as_deref(), Some("#fabd2f"));
    bar.clear_active_custom_color();
    assert_eq!(bar.active_custom_color(), None);
}

/// Id-addressed title and color edits change only the named tab, active or not,
/// and keep the empty-name rule.
#[test]
fn id_addressed_edits_change_only_the_named_tab() {
    let mut bar = TabBar::new();
    let edited = bar.push(Tab::new("#1 ~/work"));
    let active = bar.push(Tab::new("#2 ~/active"));
    assert_eq!(bar.active().map(|tab| tab.id), Some(active));

    assert!(bar.set_custom_title(edited, "renamed"));
    assert!(bar.set_custom_color(edited, "#fabd2f"));
    assert_eq!(bar.tabs[0].custom_title.as_deref(), Some("renamed"));
    assert_eq!(bar.tabs[0].title, "#1 renamed");
    assert_eq!(bar.tabs[0].custom_color.as_deref(), Some("#fabd2f"));
    assert_eq!(bar.tabs[1].custom_title, None);
    assert_eq!(bar.tabs[1].custom_color, None);

    // A blank name still restores the automatic title.
    assert!(bar.set_custom_title(edited, "  "));
    assert_eq!(bar.tabs[0].custom_title, None);
    assert_eq!(bar.tabs[0].title, "#1 ~/work");
    assert!(bar.clear_custom_color(edited));
    assert_eq!(bar.tabs[0].custom_color, None);
}

/// Edits addressed to a closed tab's id change nothing, including the tab that took its slot.
#[test]
fn id_addressed_edits_to_a_closed_tab_change_nothing() {
    let mut bar = TabBar::new();
    let closed = bar.push(Tab::new("#1 ~/closed"));
    bar.push(Tab::new("#2 ~/survivor"));
    bar.activate(0);
    bar.close(closed);

    assert!(!bar.set_custom_title(closed, "renamed"));
    assert!(!bar.set_custom_color(closed, "#fabd2f"));
    assert!(!bar.clear_custom_color(closed));
    let survivor = bar.active().unwrap();
    assert_eq!(survivor.custom_title, None);
    assert_eq!(survivor.custom_color, None);
}

#[test]
fn command_badges_respect_activity_delay_exit_status_and_expiry() {
    let now = Instant::now();

    assert_eq!(CommandStatus::Running(now - Duration::from_secs(6)).badge(now, false), Some("…"));
    assert_eq!(CommandStatus::Running(now - Duration::from_secs(6)).badge(now, true), None);
    assert_eq!(CommandStatus::Running(now - Duration::from_secs(5)).badge(now, false), None);

    let future = now + Duration::from_secs(1);
    assert_eq!(CommandStatus::Done { exit: Some(0), until: future }.badge(now, false), Some("✓"));
    assert_eq!(CommandStatus::Done { exit: Some(1), until: future }.badge(now, false), Some("✗"));
    assert_eq!(CommandStatus::Done { exit: None, until: future }.badge(now, false), Some("✗"));
    assert_eq!(CommandStatus::Done { exit: Some(0), until: now }.badge(now, false), None);
}

/// The first inactive Running badge appears at six seconds, and its deadline never repeats once due.
#[test]
fn running_visual_deadline_matches_badge_at_the_six_second_boundary() {
    let started = Instant::now();
    let threshold = Duration::from_secs(6);
    let due = started.checked_add(threshold).unwrap();
    let status = CommandStatus::Running(started);
    let unchanged = status.clone();
    for is_active in [false, true] {
        for elapsed in [
            Duration::ZERO,
            Duration::from_secs(5),
            threshold - Duration::from_nanos(1),
            threshold,
            threshold + Duration::from_nanos(1),
            Duration::from_secs(60),
        ] {
            let now = started.checked_add(elapsed).unwrap();
            let deadline = (!is_active && elapsed < threshold).then_some(due);
            let badge = (!is_active && elapsed >= threshold).then_some("…");
            assert_eq!(status.next_visual_deadline(now, is_active), deadline);
            assert_eq!(status.clone().badge(now, is_active), badge);
            assert_eq!(status.next_visual_deadline(now, is_active), deadline, "repeated query");
            assert_eq!(status, unchanged, "deadline queries must not mutate Running");
        }
    }
}

/// Every Done badge expires exactly at until, active or inactive, without rearming the expired deadline.
#[test]
fn done_visual_deadline_matches_badge_at_expiry_for_every_exit_status() {
    let now = Instant::now();
    let until = now.checked_add(Duration::from_secs(3)).unwrap();
    for (exit, visible_badge) in [(Some(0), "✓"), (Some(1), "✗"), (None, "✗")] {
        let status = CommandStatus::Done { exit, until };
        let unchanged = status.clone();
        for is_active in [false, true] {
            for at in [
                until.checked_sub(Duration::from_nanos(1)).unwrap(),
                until,
                until.checked_add(Duration::from_nanos(1)).unwrap(),
                until.checked_add(Duration::from_secs(60)).unwrap(),
            ] {
                let deadline = (at < until).then_some(until);
                let badge = (at < until).then_some(visible_badge);
                assert_eq!(status.next_visual_deadline(at, is_active), deadline);
                assert_eq!(status.clone().badge(at, is_active), badge);
                assert_eq!(status.next_visual_deadline(at, is_active), deadline, "repeated query");
                assert_eq!(status, unchanged, "deadline queries must not expire or mutate Done");
            }
        }
    }
}

/// Idle has neither a painted badge nor a future transition, independent of activity or elapsed time.
#[test]
fn idle_visual_deadline_and_badge_are_always_absent() {
    let now = Instant::now();
    let status = CommandStatus::Idle;
    for is_active in [false, true] {
        for elapsed in [Duration::ZERO, Duration::from_secs(6), Duration::from_secs(60)] {
            let at = now.checked_add(elapsed).unwrap();
            assert_eq!(status.next_visual_deadline(at, is_active), None);
            assert_eq!(status.clone().badge(at, is_active), None);
            assert_eq!(status, CommandStatus::Idle);
        }
    }
}

#[test]
fn clearing_badges_expires_only_completed_commands_at_or_before_now() {
    let now = Instant::now();
    let mut bar = TabBar::new();
    bar.push(Tab::new("running"));
    bar.push(Tab::new("expired"));
    bar.push(Tab::new("future"));
    bar.set_command_status(0, CommandStatus::Running(now - Duration::from_secs(10)));
    bar.set_command_status(1, CommandStatus::Done { exit: Some(0), until: now });
    bar.set_command_status(
        2,
        CommandStatus::Done { exit: Some(1), until: now + Duration::from_secs(1) },
    );

    bar.clear_expired_command_badges(now);

    assert!(matches!(bar.tabs[0].command, CommandStatus::Running(_)));
    assert_eq!(bar.tabs[1].command, CommandStatus::Idle);
    assert!(matches!(bar.tabs[2].command, CommandStatus::Done { exit: Some(1), .. }));
}

#[test]
fn closing_tabs_keeps_the_same_surviving_tab_active_and_renumbers_titles() {
    let mut bar = TabBar::new();
    let first = bar.push(Tab::new("#8 first"));
    let second = bar.push(Tab::new("#9 second"));
    bar.push(Tab::new("#10 third"));
    bar.activate(1);

    bar.close(first);

    assert_eq!(bar.active().map(|tab| tab.id), Some(second));
    assert_eq!(bar.active_index(), 0);
    assert_eq!(
        bar.tabs.iter().map(|tab| tab.title.as_str()).collect::<Vec<_>>(),
        vec!["#1 second", "#2 third"]
    );

    bar.close(second);
    assert_eq!(bar.active_index(), 0);
    assert_eq!(bar.active().map(|tab| tab.title.as_str()), Some("#1 third"));
}

#[test]
fn reorder_tracks_tab_identity_when_other_tabs_cross_the_active_slot() {
    let mut bar = TabBar::new();
    bar.push(Tab::new("#1 A"));
    let active = bar.push(Tab::new("#2 B"));
    bar.push(Tab::new("#3 C"));
    bar.push(Tab::new("#4 D"));
    bar.activate(1);

    bar.reorder(3, 0);
    assert_eq!(bar.active().map(|tab| tab.id), Some(active));
    assert_eq!(bar.active_index(), 2);

    bar.reorder(0, 3);
    assert_eq!(bar.active().map(|tab| tab.id), Some(active));
    assert_eq!(bar.active_index(), 1);

    bar.reorder(1, 3);
    assert_eq!(bar.active().map(|tab| tab.id), Some(active));
    assert_eq!(bar.active_index(), 3);
    assert_eq!(
        bar.tabs.iter().map(|tab| tab.title.as_str()).collect::<Vec<_>>(),
        vec!["#1 A", "#2 C", "#3 D", "#4 B"]
    );
}

#[test]
fn insertion_clamps_to_the_end_and_makes_the_inserted_tab_active() {
    let mut bar = TabBar::new();
    bar.push(Tab::new("#8 A"));
    bar.push(Tab::new("#9 B"));
    let inserted = bar.insert(usize::MAX, Tab::new("#20 C"));

    assert_eq!(bar.active().map(|tab| tab.id), Some(inserted));
    assert_eq!(bar.active_index(), 2);
    assert_eq!(
        bar.tabs.iter().map(|tab| tab.title.as_str()).collect::<Vec<_>>(),
        vec!["#1 A", "#2 B", "#3 C"]
    );
}

#[test]
fn detaching_an_inactive_tab_to_the_left_keeps_the_same_tab_active() {
    let mut bar = TabBar::new();
    let first = bar.push(Tab::new("#1 A"));
    let active = bar.push(Tab::new("#2 B"));
    bar.push(Tab::new("#3 C"));
    bar.activate(1);

    let detached = bar.detach(first).expect("first tab should detach");

    assert_eq!(detached.id, first);
    assert_eq!(bar.active().map(|tab| tab.id), Some(active));
    assert_eq!(bar.active_index(), 0);
    assert_eq!(
        bar.tabs.iter().map(|tab| tab.title.as_str()).collect::<Vec<_>>(),
        vec!["#1 B", "#2 C"]
    );
}

#[test]
fn renumbering_preserves_the_automatic_title_behind_a_custom_title() {
    let folder = '\u{f07b}';
    let mut bar = TabBar::new();
    bar.push(Tab::new(format!("#8 {folder} work/project")));
    bar.push(Tab::new(format!("#9 {folder} other/path")));
    bar.activate(0);
    bar.set_active_custom_title("renamed");

    bar.reorder(0, 1);

    assert_eq!(
        bar.active().map(|tab| tab.title.as_str()),
        Some(format!("#2 {folder} renamed").as_str())
    );
    bar.set_active_custom_title(" ");
    assert_eq!(
        bar.active().map(|tab| tab.title.as_str()),
        Some(format!("#2 {folder} work/project").as_str())
    );
}

#[test]
fn title_body_helpers_preserve_icons_and_handle_unformatted_titles() {
    let folder = '\u{f07b}';
    assert_eq!(
        title_with_replaced_body(&format!("#12 {folder} old/path"), "renamed"),
        format!("#12 {folder} renamed")
    );
    assert_eq!(title_with_replaced_body("#2 ~/work", "renamed"), "#2 renamed");
    assert_eq!(title_with_replaced_body("Welcome", "renamed"), "renamed");

    let mut bar = TabBar::new();
    bar.push(Tab::new(format!("#1 {folder} old/path")));
    assert_eq!(bar.active_title_body().as_deref(), Some("old/path"));
    bar.set_active_custom_title("custom");
    assert_eq!(bar.active_title_body().as_deref(), Some("custom"));
}

/// Stand-in for the tab font: every display column advances `COLUMN_PX` and every scalar
/// is its own shaping cluster, the way a monospace terminal font draws.
const COLUMN_PX: f32 = 10.0;

fn column_advances(text: &str) -> Vec<(usize, f32)> {
    text.char_indices()
        .map(|(offset, character)| (offset, character.width().unwrap_or(0) as f32 * COLUMN_PX))
        .collect()
}

fn column_width(content: &TabContent<'_>) -> Option<f32> {
    Some(column_advances(&content.display_text()).iter().map(|(_, advance)| advance).sum())
}

fn doubled_column_width(content: &TabContent<'_>) -> Option<f32> {
    column_width(content).map(|width_px| width_px * 2.0)
}

fn marked_doubled_column_width(content: &TabContent<'_>) -> Option<f32> {
    let marker_px = if content.privileged { 24.0 } else { 0.0 };
    doubled_column_width(content).map(|width_px| width_px + marker_px)
}

fn fit_columns(text: &str, available_px: f32) -> FittedTitle {
    fit_title_to_width(text, &column_advances(text), COLUMN_PX, available_px)
}

#[test]
fn content_width_counts_the_command_badge_only_while_it_is_shown() {
    // A stored width follows the drawn text: the running `…` shows only on an inactive
    // tab after five seconds, and `✓` only until its deadline passes. The bar records when
    // it measured, so drawing judges the badge at that same instant.
    let start = Instant::now();
    let mut bar = TabBar::new();
    bar.push(Tab::new("build"));
    bar.push(Tab::new("shell"));
    bar.set_command_status(0, CommandStatus::Running(start));
    let refresh_at = |bar: &mut TabBar, now: Instant| {
        bar.refresh_content_widths(now, false, 1, false, column_width);
        bar.tabs()[0].content_width_px()
    };

    assert_eq!(refresh_at(&mut bar, start + Duration::from_secs(3)), Some(50.0));
    assert_eq!(refresh_at(&mut bar, start + Duration::from_secs(7)), Some(70.0));
    bar.activate(0);
    assert_eq!(refresh_at(&mut bar, start + Duration::from_secs(8)), Some(50.0));
    let until = start + Duration::from_secs(20);
    bar.set_command_status(0, CommandStatus::Done { exit: Some(0), until });
    assert_eq!(refresh_at(&mut bar, start + Duration::from_secs(9)), Some(70.0));
    assert_eq!(refresh_at(&mut bar, until), Some(50.0));
    assert_eq!(bar.content_measured_at(), Some(until));
}

#[test]
fn unchanged_content_is_not_measured_again() {
    // Re-shaping every title on every frame is wasted work: only a changed title, badge,
    // privilege marker or font is measured again.
    let now = Instant::now();
    let mut bar = TabBar::new();
    bar.push(Tab::new("zsh"));
    bar.push(Tab::new("vim"));

    let first = bar.refresh_content_widths(now, false, 1, false, column_width);
    assert_eq!(first, ContentWidthRefresh { measured: 2, applied: 2, held: 0 });
    let again = bar.refresh_content_widths(now, false, 1, false, column_width);
    assert_eq!(again, ContentWidthRefresh::default());

    let id = bar.tabs()[0].id;
    bar.set_title(id, "cargo test");
    let retitled = bar.refresh_content_widths(now, false, 1, false, column_width);
    assert_eq!(retitled, ContentWidthRefresh { measured: 1, applied: 1, held: 0 });
    assert_eq!(bar.tabs()[0].content_width_px(), Some(100.0));
    let rescaled = bar.refresh_content_widths(now, false, 2, false, column_width);
    assert_eq!(rescaled, ContentWidthRefresh { measured: 2, applied: 2, held: 0 });
}

#[test]
fn a_held_bar_keeps_its_widths_until_it_is_released() {
    // A title change under a pressed, dragged or hovered bar must not move a tab under the
    // pointer: the new width is measured once and waits for the bar to be released.
    let now = Instant::now();
    let mut bar = TabBar::new();
    bar.push(Tab::new("zsh"));
    bar.push(Tab::new("vim"));
    bar.refresh_content_widths(now, false, 1, false, column_width);
    let id = bar.tabs()[0].id;
    bar.set_title(id, "cargo build --release");

    let held = bar.refresh_content_widths(now, false, 1, true, column_width);
    assert_eq!(held, ContentWidthRefresh { measured: 1, applied: 0, held: 1 });
    assert_eq!(bar.tabs()[0].content_width_px(), Some(30.0));
    assert!(bar.has_held_content_widths());
    let still_held = bar.refresh_content_widths(now, false, 1, true, column_width);
    assert_eq!(still_held, ContentWidthRefresh { measured: 0, applied: 0, held: 1 });

    let released = bar.refresh_content_widths(now, false, 1, false, column_width);
    assert_eq!(released, ContentWidthRefresh { measured: 0, applied: 1, held: 0 });
    assert_eq!(bar.tabs()[0].content_width_px(), Some(210.0));
    assert!(!bar.has_held_content_widths());
}

#[test]
fn font_scale_and_new_tabs_lay_out_at_once_even_while_held() {
    // A font or DPI reload moves every tab anyway and a new tab has no width to keep, so
    // both lay out immediately; a privilege-marker change on a held bar waits for release.
    let now = Instant::now();
    let mut bar = TabBar::new();
    bar.push(Tab::new("zsh"));
    bar.refresh_content_widths(now, false, 1, false, column_width);

    let reloaded = bar.refresh_content_widths(now, false, 2, true, doubled_column_width);
    assert_eq!(reloaded, ContentWidthRefresh { measured: 1, applied: 1, held: 0 });
    assert_eq!(bar.tabs()[0].content_width_px(), Some(60.0));
    bar.push(Tab::new("vim"));
    let opened = bar.refresh_content_widths(now, false, 2, true, doubled_column_width);
    assert_eq!(opened, ContentWidthRefresh { measured: 1, applied: 1, held: 0 });
    assert_eq!(bar.tabs()[1].content_width_px(), Some(60.0));

    let elevated = bar.refresh_content_widths(now, true, 2, true, marked_doubled_column_width);
    assert_eq!(elevated, ContentWidthRefresh { measured: 2, applied: 0, held: 2 });
    assert_eq!(bar.tabs()[0].content_width_px(), Some(60.0));
}

#[test]
fn a_failed_measurement_keeps_the_last_good_width() {
    // A shaping failure must not collapse a measured tab to the readable minimum: the tab
    // keeps its last good width and the next pass measures again.
    let now = Instant::now();
    let mut bar = TabBar::new();
    bar.push(Tab::new("zsh"));
    bar.refresh_content_widths(now, false, 1, false, column_width);
    let id = bar.tabs()[0].id;
    bar.set_title(id, "nvim");

    let failed = bar.refresh_content_widths(now, false, 1, false, |_| None);
    assert_eq!(failed, ContentWidthRefresh::default());
    assert_eq!(bar.tabs()[0].content_width_px(), Some(30.0));
    let retried = bar.refresh_content_widths(now, false, 1, false, column_width);
    assert_eq!(retried, ContentWidthRefresh { measured: 1, applied: 1, held: 0 });
    assert_eq!(bar.tabs()[0].content_width_px(), Some(40.0));
}

#[test]
fn a_title_that_fits_is_drawn_whole_despite_sub_pixel_rounding() {
    // Layout arithmetic can leave a rect a fraction of a pixel short of the width it was
    // sized for; that must not cut a title that fits.
    let title = "#3 ~/work/sonicterm";

    assert_eq!(
        fit_columns(title, 190.0),
        FittedTitle { text: title.to_string(), width_px: 190.0, cut: false }
    );
    assert_eq!(fit_columns(title, 189.7).text, title);
    assert!(fit_columns(title, 180.0).cut);
}

#[test]
fn a_cut_title_keeps_its_index_process_icon_and_command_badge() {
    // Cutting only the body keeps the tab identifiable: `#N`, the process icon and the
    // status badge survive whenever they fit beside the ellipsis.
    let folder = '\u{f07b}';
    let stored = format!("#12 {folder} workspace/project");
    let display = format!("✓ {stored}");

    assert_eq!(fit_columns(&display, 140.0).text, format!("✓ #12 {folder} works…"));
    assert_eq!(fit_columns(&stored, 120.0).text, format!("#12 {folder} works…"));
    assert_eq!(fit_columns(&display, 50.0).text, "✓ #1…");
    assert_eq!(fit_columns("abcdefgh", 50.0).text, "abcd…");
}

#[test]
fn a_cut_never_splits_a_wide_character_an_emoji_sequence_or_a_combining_mark() {
    // A double-width CJK character counts two columns, and the cut lands on a grapheme
    // boundary, so a joined emoji or an accented letter is kept or dropped whole.
    assert_eq!(fit_columns("任务完成", 55.0).text, "任务…");
    assert_eq!(fit_columns("任务完成", 35.0).text, "任…");
    let coder = "ab\u{1f469}\u{200d}\u{1f4bb}cd";
    assert_eq!(fit_columns(coder, 40.0).text, "ab…");
    assert_eq!(fit_columns(coder, 70.0).text, "ab\u{1f469}\u{200d}\u{1f4bb}…");
    assert_eq!(fit_columns("cafe\u{301}s!", 55.0).text, "cafe\u{301}…");
}

#[test]
fn nothing_is_drawn_when_even_the_ellipsis_does_not_fit() {
    // A clipped glyph reads as noise, so a rect narrower than `…` draws nothing.
    assert_eq!(
        fit_columns("zsh", 9.0),
        FittedTitle { text: String::new(), width_px: 0.0, cut: true }
    );
}

#[test]
fn title_truncation_does_not_mutate_automatic_or_custom_title_state() {
    // Protect rename state from absorbing a renderer-only measured cut.
    let folder = '\u{f07b}';
    let mut bar = TabBar::new();
    bar.push(Tab::new(format!("#1 {folder} workspace/project")));
    bar.set_active_custom_title("renamed project");
    let before = bar.active().expect("active tab").clone();

    let _ = fit_columns(&before.title, 100.0);

    let after = bar.active().expect("active tab");
    assert_eq!(after.title, before.title);
    assert_eq!(after.auto_title, before.auto_title);
    assert_eq!(after.custom_title, before.custom_title);
}

#[test]
fn foreground_privilege_is_independent_tab_state_and_survives_rename_and_detach() {
    // Protect a gsudo warning from being stored in or erased by editable title text.
    let mut bar = TabBar::new();
    let id = bar.push(Tab::new("#1 shell"));

    assert!(bar.set_active_foreground_privileged(true));
    assert!(bar.active().expect("active tab").foreground_privileged);
    assert_eq!(bar.active().expect("active tab").title, "#1 shell");

    bar.set_active_custom_title("renamed");
    assert!(bar.active().expect("active tab").foreground_privileged);
    assert_eq!(bar.active().expect("active tab").title, "#1 renamed");
    assert!(!bar.set_active_foreground_privileged(true));

    let detached = bar.detach(id).expect("tab detaches");
    assert!(detached.foreground_privileged);
    assert_eq!(detached.custom_title.as_deref(), Some("renamed"));
}

#[test]
fn each_tab_keeps_its_own_foreground_privilege_state() {
    // Protect one gsudo tab from warning every tab in a regular SonicTerm process.
    let mut bar = TabBar::new();
    bar.push(Tab::new("#1 regular"));
    bar.push(Tab::new("#2 gsudo"));
    assert!(bar.set_active_foreground_privileged(true));
    bar.activate(0);

    assert!(!bar.tabs()[0].foreground_privileged);
    assert!(bar.tabs()[1].foreground_privileged);
}

#[test]
fn indexed_foreground_privilege_updates_an_inactive_tab_without_moving_focus() {
    // Protect background gsudo state from being written through the active-tab selector.
    let mut bar = TabBar::new();
    let active = bar.push(Tab::new("#1 regular"));
    bar.push(Tab::new("#2 background"));
    bar.activate(0);

    assert!(bar.set_foreground_privileged(1, true));

    assert_eq!(bar.active().map(|tab| tab.id), Some(active));
    assert_eq!(bar.tabs()[0].title, "#1 regular");
    assert_eq!(bar.tabs()[1].title, "#2 background");
    assert!(!bar.tabs()[0].foreground_privileged);
    assert!(bar.tabs()[1].foreground_privileged);
    assert!(!bar.set_foreground_privileged(1, true));
}

#[test]
fn activation_counts_only_changes_of_the_active_tab() {
    // A switch and a switch back both count, so A to B to A is not mistaken for no switch;
    // re-activating the current tab, reordering, and closing a background tab do not count.
    let mut bar = TabBar::new();
    bar.push(Tab::new("a"));
    bar.push(Tab::new("b"));
    bar.activate(0);
    let start = bar.activation();
    bar.activate(0);
    assert_eq!(bar.activation(), start, "the active tab did not change");
    bar.activate(1);
    bar.activate(0);
    assert_eq!(bar.activation(), start + 2, "A to B and back counts twice");
    bar.push(Tab::new("c"));
    bar.activate(0);
    let settled = bar.activation();
    bar.reorder(1, 2);
    let background = bar.tabs()[2].id;
    bar.close(background);
    assert_eq!(bar.activation(), settled, "reordering and a background close keep the tab");
    let active = bar.tabs()[bar.active_index()].id;
    bar.close(active);
    assert_eq!(bar.activation(), settled + 1, "closing the active tab moves to a neighbour");
}
