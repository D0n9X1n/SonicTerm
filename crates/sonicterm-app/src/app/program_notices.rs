//! One batch's program notifications, kept bounded before they reach the App: one per occasion,
//! so a notice a later rule would suppress never hides one that should show.

use sonicterm_vt::vt::NotifyOccasion;

use super::super::UserEvent;

/// One kept notification of a batch and how many followed it with the same occasion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) struct Notice {
    pub(in crate::app) title: Option<String>,
    pub(in crate::app) body: String,
    pub(in crate::app) id: Option<String>,
    pub(in crate::app) occasion: NotifyOccasion,
    pub(in crate::app) more: u32,
}

impl Notice {
    /// The event that hands this notice to the App for `pane_id`.
    pub(in crate::app) fn into_event(self, pane_id: u64) -> UserEvent {
        let Self { title, body, id, occasion, more } = self;
        UserEvent::ProgramNotify { pane_id, title, body, id, occasion, more }
    }
}

/// A batch's notifications: per occasion the first one with text, whole, and the rest only
/// counted, except that a later one with the kept one's id replaces it. No coalesced text is kept.
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::app) struct ProgramNotices {
    slots: [Option<Notice>; 3],
}

impl ProgramNotices {
    /// Record one OSC 9 or 777 notification, which is always shown and has no id.
    #[cfg(test)]
    pub(in crate::app) fn push(&mut self, title: Option<String>, body: String) {
        self.push_notice(title, body, None, NotifyOccasion::Always);
    }

    /// Record one notification; one that sanitizes to no text is neither shown nor counted.
    pub(in crate::app) fn push_notice(
        &mut self,
        title: Option<String>,
        body: String,
        id: Option<String>,
        occasion: NotifyOccasion,
    ) {
        if !super::super::program_state::has_visible_text(title.as_deref(), &body) {
            // When: has_visible_text is false, the App would show nothing, so the notification
            // must not take its occasion's place or its alert mark.
            return;
        }
        let slot = &mut self.slots[occasion as usize];
        match slot {
            // A notification that repeats the kept one's id replaces its text.
            Some(kept) if id.is_some() && kept.id == id => {
                kept.title = title;
                kept.body = body;
            }
            Some(kept) => kept.more = kept.more.saturating_add(1),
            None => *slot = Some(Notice { title, body, id, occasion, more: 0 }),
        }
    }

    /// The kept notifications, at most one per occasion, in occasion order.
    pub(in crate::app) fn into_notices(self) -> impl Iterator<Item = Notice> {
        self.slots.into_iter().flatten()
    }

    /// The `Always` notification's title, body and following count, if any arrived.
    #[cfg(test)]
    pub(in crate::app) fn into_parts(self) -> Option<(Option<String>, String, u32)> {
        let [always, ..] = self.slots;
        always.map(|notice| (notice.title, notice.body, notice.more))
    }
}

#[cfg(test)]
#[path = "program_notices_tests.rs"]
mod program_notices_tests;
