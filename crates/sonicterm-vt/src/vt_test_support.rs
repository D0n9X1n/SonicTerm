use super::{MediaEvent, Parser, VtEvent};

pub(super) fn row_text(parser: &Parser, row: u16) -> String {
    parser.grid().row(row).iter().map(|cell| cell.ch).collect()
}

pub(super) fn only_media(events: Vec<VtEvent>) -> MediaEvent {
    let mut media = events.into_iter().filter_map(|event| match event {
        VtEvent::Media(media) => Some(media),
        _ => None,
    });
    let event = media.next().expect("one media event");
    assert!(media.next().is_none(), "expected exactly one media event");
    event
}
