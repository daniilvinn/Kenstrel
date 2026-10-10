//! Event sinks: where engines deliver their events.

use std::sync::mpsc::Sender;

/// Receives events from an engine. Called from engine threads, so it must be cheap and must not
/// block (forward into a channel and return).
///
/// Implemented for [`std::sync::mpsc::Sender`] and for any `Fn(E) + Send + Sync + 'static`
/// closure (e.g. one that wraps a tokio `UnboundedSender` and sends a `Msg`).
pub trait EventSink<E>: Send + Sync + 'static {
    /// Deliver one event. Delivery failures (receiver gone) are ignored by the engine.
    fn send(&self, event: E);
}

impl<E: Send + 'static> EventSink<E> for Sender<E> {
    fn send(&self, event: E) {
        // A closed receiver just means nobody listens any more.
        let _ = Sender::send(self, event);
    }
}

impl<E, F> EventSink<E> for F
where
    F: Fn(E) + Send + Sync + 'static,
{
    fn send(&self, event: E) {
        self(event);
    }
}
