//! The actual ready server association, retaining its original receivers through local close.

use super::Inbox;
use bytes::Bytes;
use nethernet_tokio::{AcceptedSession, Retired};
use std::{io, time::Instant};

pub(in crate::login::native_network) struct AcceptedTransport {
    state: Option<State>,
    inbox: Inbox,
}
enum State {
    Established(AcceptedSession),
    Retired(AcceptedSession<Retired>),
}

impl AcceptedTransport {
    /// Takes an existing Ready association; no new socket, dial or signaling route is created.
    pub(super) fn new(accepted: AcceptedSession) -> Self {
        Self {
            state: Some(State::Established(accepted)),
            inbox: Inbox::default(),
        }
    }

    /// Drives original accepted input beside the same bounded actor's send admission.
    pub(super) async fn send_until(
        &mut self,
        data: Bytes,
        deadline: Option<Instant>,
    ) -> io::Result<()> {
        let State::Established(accepted) =
            self.state.as_mut().expect("synchronous phase transition")
        else {
            return Err(super::closing());
        };
        let sender = accepted.session.sender();
        let send = async move {
            match deadline {
                Some(deadline) => sender.send_before(data, deadline).await,
                None => sender.send(data).await,
            }
            .map_err(io::Error::other)
        };
        let incoming = futures::stream::unfold(&mut accepted.reliable, |receiver| async move {
            match receiver.recv().await {
                Ok(Some(data)) => Some((Ok(data), receiver)),
                Ok(None) => None,
                Err(error) => Some((Err(io::Error::other(error)), receiver)),
            }
        });
        self.inbox.send(send, incoming).await
    }

    /// Drains the accepted write prefix before the original established/retired receive FIFO.
    pub(super) async fn recv(&mut self) -> io::Result<Bytes> {
        if let Some(data) = self.inbox.pop() {
            return Ok(data);
        }
        let result = match self.state.as_mut().expect("synchronous phase transition") {
            State::Established(accepted) => accepted.reliable.recv().await,
            State::Retired(accepted) => accepted.reliable.recv().await,
        };
        result.map_err(io::Error::other)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "accepted NetherNet peer closed",
            )
        })
    }

    /// Consumes established admission without replacing or dropping either accepted receiver.
    pub(super) fn close(&mut self) {
        self.state = Some(
            match self.state.take().expect("synchronous phase transition") {
                State::Established(accepted) => State::Retired(accepted.close()),
                retired @ State::Retired(_) => retired,
            },
        );
    }
}
