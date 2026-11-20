//! Windows desktop notifications through `notify-rust`, whose `show` blocks; it runs on its own
//! thread so the worker can stop waiting at the bound.

use super::{DeliveryOutcome, DesktopNotice, NotifyBackend};

/// The Windows toast service.
pub(crate) struct WindowsToasts;

impl NotifyBackend for WindowsToasts {
    fn deliver(&mut self, notice: DesktopNotice, done: Box<dyn FnOnce(DeliveryOutcome) + Send>) {
        let spawned = std::thread::Builder::new().name("sonicterm-toast".into()).spawn(move || {
            let shown =
                notify_rust::Notification::new().summary(&notice.title).body(&notice.body).show();
            done(match shown {
                Ok(_) => DeliveryOutcome::Delivered,
                Err(_) => DeliveryOutcome::Failed("the toast service refused the request"),
            });
        });
        // A request no thread could run is dropped with `done`, which the worker reads at once.
        if spawned.is_err() {
            tracing::debug!(target: "sonicterm_app::app", "desktop notification thread did not start");
        }
    }

    fn quarantines(&self) -> bool {
        // A blocked `show` cannot be withdrawn, so a late one must finish first.
        true
    }
}
