//! The page's single relay driver. Host observations own clocks, timers and presentation.
use futures::{
    FutureExt, StreamExt,
    channel::{mpsc, oneshot},
};
use std::{cell::Cell, future::Future, rc::Rc, time::Duration};
use web_transport::{
    CloseReason, RelayTransport,
    control::refusal_message,
    session::{AccountOperation, ControlEvent, Session, State},
};

/// Bounds control opening and account writes with the same host-owned deadline.
const CONTROL_DEADLINE: Duration = Duration::from_secs(10);

use crate::play;

/// Explicit UI intent; restoring a page retains its account capability.
#[derive(Clone, Copy, Default)]
pub(crate) enum Refresh {
    #[default]
    None,
    Reconnect,
    Fresh,
}

/// Browser services supplied only at composition, keeping the driver testable without JS.
pub(crate) trait Host {
    type Transport: RelayTransport<Stream: 'static> + 'static;
    /// Samples the host monotonic clock.
    fn now(&self) -> Duration;
    /// Supplies one bounded retry offset.
    fn jitter(&self) -> Duration;
    /// Registers a timer cancelled when its future is dropped.
    fn sleep(&self, duration: Duration) -> impl Future<Output = ()>;
    /// Creates candidates without performing a second account or gameplay implementation.
    fn transports(&self) -> Result<(Option<Self::Transport>, Self::Transport), CloseReason>;
    /// Reconciles menu presentation after a fresh or resumed account handshake.
    fn connected(&self, _resumed: bool, _pending: Option<AccountOperation>) {}
    /// Mirrors the admitted coalesced account operation into menu presentation.
    fn account_operation(&self, _operation: AccountOperation) {}
    /// Supplies the menu's account owner with the same validated control observation.
    fn control(&self, _event: &ControlEvent) {}
    /// Emits only typed, sanitized page observations.
    fn observe(&self, event: serde_json::Value);
}

/// Runs selection, backoff and account control on one retained session until disposal.
pub(crate) async fn run<H: Host>(
    host: H,
    mut pending: mpsc::Receiver<AccountOperation>,
    mut joins: mpsc::Receiver<play::Request<<H::Transport as RelayTransport>::Stream>>,
    mut changed: mpsc::Receiver<()>,
    paused: Rc<Cell<bool>>,
    refresh: Rc<Cell<Refresh>>,
    cancelled: oneshot::Receiver<()>,
) {
    let mut session = Session::new();
    let mut cancelled = cancelled.fuse();
    let mut halted = false;
    let mut requested: Option<play::Request<<H::Transport as RelayTransport>::Stream>> = None;
    let mut playing: Option<play::Active> = None;
    let sign_out_deadline = Cell::new(None::<Duration>);
    loop {
        let ready = matches!(session.state(), State::Ready(_) | State::Playing(_));
        let writing = ready && session.has_account_intent();
        let opening = requested.is_some();
        let parked = paused.get()
            || (!ready
                && ((halted && !session.has_account_intent()) || session.next_retry().is_err()));
        enum Action<S> {
            Dispose,
            Lifecycle,
            Account(Option<AccountOperation>),
            Join(Option<play::Request<S>>),
            PlayEnded,
            Step(Result<(), CloseReason>),
        }
        let mut started = None;
        let mut queued_account = None;
        let action = {
            let handshaking = Cell::new(false);
            let step = async {
                if parked {
                    return std::future::pending::<Result<(), CloseReason>>().await;
                }
                if writing {
                    bridge::tasks::until(session.flush_account(), host.sleep(CONTROL_DEADLINE))
                        .await
                        .unwrap_or(Err(CloseReason::Transport))?;
                    return Ok(());
                }
                if let Some(request) = requested.take() {
                    if request.reply.is_canceled() {
                        return Ok(());
                    }
                    if !matches!(session.state(), State::Ready(_)) {
                        let _ = request.reply.send(Err(CloseReason::Protocol));
                        return Ok(());
                    }
                    match session
                        .open_play(|| host.now(), host.sleep(CONTROL_DEADLINE))
                        .await
                    {
                        Ok((reader, frames, writer)) => {
                            let (ended, finished) = oneshot::channel();
                            let (cancel, cancelled) = oneshot::channel();
                            started = Some(play::Active {
                                finished,
                                cancel: Some(cancel),
                            });
                            let _ = request.reply.send(Ok(play::Lease {
                                io: play::Io {
                                    reader,
                                    frames,
                                    writer,
                                },
                                ended,
                                cancelled,
                            }));
                        }
                        Err(reason) => {
                            let _ = request.reply.send(Err(reason));
                            return Err(reason);
                        }
                    }
                    return Ok(());
                }
                if ready {
                    let event = session.next_event(|| host.now());
                    let event = if let Some(deadline) = sign_out_deadline.get() {
                        bridge::tasks::until(event, host.sleep(deadline.saturating_sub(host.now())))
                            .await
                            .unwrap_or(Err(CloseReason::Transport))?
                    } else {
                        event.await?
                    };
                    host.control(&event);
                    match event {
                        ControlEvent::Account(account) => {
                            if matches!(
                                account.state,
                                bridge::contract::AuthState::SignedOut
                                    | bridge::contract::AuthState::Failed
                            ) {
                                sign_out_deadline.set(None);
                            }
                            host.observe(
                                serde_json::json!({"state": account.state, "account": account}),
                            );
                        }
                        ControlEvent::Connect(connect) => {
                            host.observe(serde_json::json!({"connect": connect}))
                        }
                        ControlEvent::Error(code) => host.observe(
                            serde_json::json!({"code": code, "error": refusal_message(code)}),
                        ),
                        ControlEvent::Keepalive => {}
                    }
                } else {
                    let due = session.next_retry()?;
                    if due > host.now() {
                        host.sleep(due.saturating_sub(host.now())).await;
                    }
                    if host.now() < due {
                        return Err(CloseReason::RetryAt(due));
                    }
                    let (quic, socket) = host.transports()?;
                    handshaking.set(true);
                    web_transport::select(
                        &mut session,
                        quic,
                        || socket,
                        || host.now(),
                        || host.sleep(CONTROL_DEADLINE),
                        || host.jitter(),
                    )
                    .await?;
                    host.connected(session.resumed_account(), session.pending_account());
                    host.observe(serde_json::json!({"state": "connected"}));
                }
                Ok(())
            };
            futures::pin_mut!(step);
            loop {
                let action = futures::select_biased! {
                    _ = cancelled => Action::Dispose,
                    signal = changed.next().fuse() => if signal.is_none() { Action::Dispose } else { Action::Lifecycle },
                    command = async {
                        if writing || opening { std::future::pending().await } else { pending.next().await }
                    }.fuse() => Action::Account(command),
                    request = async {
                        if writing || opening { std::future::pending().await } else { joins.next().await }
                    }.fuse() => Action::Join(request),
                    _ = async {
                        if writing || sign_out_deadline.get().is_some() { return std::future::pending().await; }
                        match playing.as_mut() {
                            Some(active) => { let _ = (&mut active.finished).await; }
                            None => std::future::pending().await,
                        }
                    }.fuse() => Action::PlayEnded,
                    result = step.as_mut().fuse() => Action::Step(result),
                };
                match action {
                    // Keep the Hello/Welcome future alive until its bearer has been stored.
                    Action::Account(Some(operation)) if !ready && !parked => {
                        queued_account = Some(operation);
                    }
                    Action::Join(Some(request)) if !ready => {
                        let _ = request.reply.send(Err(CloseReason::Protocol));
                    }
                    Action::Lifecycle
                        if !paused.get()
                            && matches!(refresh.get(), Refresh::Reconnect)
                            && (ready || handshaking.get()) =>
                    {
                        while changed.try_recv().is_ok() {}
                        refresh.set(Refresh::None);
                        halted = false;
                    }
                    _ => break action,
                }
            }
        };
        if let Some(finished) = started {
            playing = Some(finished);
        }
        match action {
            Action::Dispose | Action::Account(None) => break,
            Action::Join(None) => break,
            Action::Join(Some(request)) => {
                if matches!(session.state(), State::Ready(_)) && playing.is_none() {
                    requested = Some(request);
                } else {
                    let _ = request.reply.send(Err(CloseReason::Protocol));
                }
            }
            Action::PlayEnded => {
                playing = None;
                let _ = session.finish_play();
            }
            Action::Lifecycle => {
                while changed.try_recv().is_ok() {}
                session.close(CloseReason::Transport);
                playing = None;
                sign_out_deadline.set(None);
                match refresh.replace(Refresh::None) {
                    Refresh::Fresh => {
                        session.fresh_session();
                        queued_account = None;
                        halted = false;
                    }
                    Refresh::Reconnect => {
                        session.restart_retries();
                        halted = false;
                    }
                    Refresh::None => {}
                }
            }
            Action::Account(Some(operation)) => queued_account = Some(operation),
            Action::Step(Ok(())) => {}
            Action::Step(Err(CloseReason::RetryAt(_))) => {}
            Action::Step(Err(reason)) => {
                session.close(reason);
                playing = None;
                sign_out_deadline.set(None);
                if let Some(code) = session.refusal() {
                    host.observe(serde_json::json!({"state": "refused", "code": code, "error": refusal_message(code)}));
                } else if session.next_retry() == Err(CloseReason::RetryExhausted) {
                    host.observe(serde_json::json!({"state": "retry_exhausted", "error": "Reconnect attempts exhausted. Connect relay to retry."}));
                } else {
                    host.observe(serde_json::json!({"state": "disconnected"}));
                }
            }
        }
        if let Some(mut operation) = queued_account {
            while let Ok(next) = pending.try_recv() {
                operation = next;
            }
            halted = operation == AccountOperation::SignOut;
            sign_out_deadline.set(halted.then(|| host.now().saturating_add(CONTROL_DEADLINE)));
            if halted && let Some(active) = playing.as_mut() {
                active.cancel.take();
            }
            host.account_operation(operation);
            session.desire_account(operation);
        }
    }
    session.close(CloseReason::Cancelled);
}

#[cfg(test)]
mod tests;
