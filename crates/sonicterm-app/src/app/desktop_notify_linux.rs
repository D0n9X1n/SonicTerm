//! Linux desktop notifications: the XDG notification portal, registered as SonicTerm's desktop
//! id, and `org.freedesktop.Notifications` where the portal has no Notification interface (as
//! with xdg-desktop-portal-wlr). Each request runs inside its bound; dropping a late one cancels
//! its D-Bus call, so nothing stays outstanding.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use super::{escape_markup, notice_identifier, DeliveryOutcome, DesktopNotice, NotifyBackend};

/// The desktop id the portal knows SonicTerm by, from its desktop entry.
const APP_ID: &str = "com.d0n9x1n.SonicTerm";

/// Server ids the fallback remembers to replace, at most this many.
const MAX_REPLACEABLE: usize = 64;

/// The portal, with the notification server as fallback.
pub(crate) struct DesktopPortal {
    bound: Duration,
    registered: bool,
    /// `org.freedesktop.Notifications` ids the server returned, by native id, so a later
    /// notification with the same id replaces it.
    replaces: HashMap<String, u32>,
}

impl DesktopPortal {
    /// A backend whose requests each finish within `bound`.
    pub(crate) fn new(bound: Duration) -> Self {
        Self { bound, registered: false, replaces: HashMap::new() }
    }

    /// Show `notice`: through the portal, or the notification server when the portal has none.
    async fn show(&mut self, notice: &DesktopNotice) -> DeliveryOutcome {
        if !self.registered {
            // When: `registered` is false, this first request registers the unsandboxed app's
            // desktop id; a portal without the Registry refuses, which changes nothing.
            self.registered = true;
            if let Ok(app_id) = ashpd::AppID::try_from(APP_ID) {
                // When: `APP_ID` parses as an `app_id`, it is registered; nothing depends on the answer.
                let _ = ashpd::register_host_app(app_id).await;
            }
        }
        let id = notice_identifier(notice);
        // The portal takes plain text, so the body is never escaped here.
        let portal = async {
            let proxy = ashpd::desktop::notification::NotificationProxy::new().await?;
            let notification = ashpd::desktop::notification::Notification::new(&notice.title)
                .body(notice.body.as_str());
            proxy.add_notification(&id, notification).await
        };
        match portal.await {
            Ok(()) => DeliveryOutcome::Delivered,
            Err(_) => self.show_on_server(notice).await,
        }
    }

    /// Show `notice` through `org.freedesktop.Notifications`, whose servers may read the body as
    /// markup, so it is escaped; a repeated native id replaces the server's earlier notification.
    async fn show_on_server(&mut self, notice: &DesktopNotice) -> DeliveryOutcome {
        let Ok(connection) = ashpd::zbus::Connection::session().await else {
            // When: there is no session bus, no notification service can be reached.
            return DeliveryOutcome::Unavailable("no session bus");
        };
        let replaces_id =
            notice.native_id.as_ref().and_then(|id| self.replaces.get(id)).copied().unwrap_or(0);
        let hints: HashMap<&str, ashpd::zbus::Variant<'_>> = HashMap::new();
        let reply = connection
            .call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &(
                    "SonicTerm",
                    replaces_id,
                    "",
                    notice.title.as_str(),
                    escape_markup(&notice.body),
                    Vec::<&str>::new(),
                    hints,
                    -1i32,
                ),
            )
            .await;
        let Ok(reply) = reply else {
            // When: `reply` is an error, no notification server answered; notifications stay in-app.
            return DeliveryOutcome::Unavailable("no notification service");
        };
        if let (Some(native_id), Ok(server_id)) =
            (&notice.native_id, reply.body().deserialize::<u32>())
        {
            // A bounded map: the oldest replaceable ids are forgotten first.
            if self.replaces.len() >= MAX_REPLACEABLE && !self.replaces.contains_key(native_id) {
                self.replaces.clear();
            }
            self.replaces.insert(native_id.clone(), server_id);
        }
        DeliveryOutcome::Delivered
    }
}

impl NotifyBackend for DesktopPortal {
    fn deliver(&mut self, notice: DesktopNotice, done: Box<dyn FnOnce(DeliveryOutcome) + Send>) {
        let bound = self.bound;
        let outcome = {
            let mut operation = std::pin::pin!(self.show(&notice));
            let mut deadline = std::pin::pin!(async_io::Timer::after(bound));
            async_io::block_on(std::future::poll_fn(|context| {
                if let std::task::Poll::Ready(outcome) = operation.as_mut().poll(context) {
                    // When: the request finished within its bound, its outcome stands.
                    return std::task::Poll::Ready(outcome);
                }
                if deadline.as_mut().poll(context).is_ready() {
                    // When: the `deadline` passed, the request is dropped, which cancels its call.
                    return std::task::Poll::Ready(DeliveryOutcome::Failed("timed out"));
                }
                std::task::Poll::Pending
            }))
        };
        done(outcome);
    }

    fn quarantines(&self) -> bool {
        // Each request is finished or dropped inside `deliver`, so none outlives it.
        false
    }
}
