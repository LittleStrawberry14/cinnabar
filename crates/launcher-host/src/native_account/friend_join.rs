//! Explicit friend-world membership, RTA progress and authoritative connection admission.

mod selection;
mod world;
pub use selection::FriendSelection;

use super::{NativeAccount, Operation};
use bedrock_services::xbox::{mpsd, rta};
use rta::websocket::{Options, RtaSocket, SocketError};
use sha2::{Digest, Sha256};
use std::{
    future::{Future, poll_fn},
    io,
    sync::Arc,
    task::Poll,
    time::Instant,
};
use uuid::Uuid;

/// One completed admission, bound to the account generation that joined the directory.
/// No Debug or serialization: the nonce authorizes this player's game login.
pub struct FriendAdmission {
    scope: Scope,
    peer: Uuid,
    nonce: zeroize::Zeroizing<String>,
}

impl FriendAdmission {
    /// Returns the joined host's Player Messaging UUID, never an activity's advertised endpoint.
    pub fn peer(&self) -> Uuid {
        self.peer
    }

    /// Checks the existing leased account immediately before the caller mints a connection credential.
    pub(super) fn validate(&self, operation: &Operation) -> io::Result<()> {
        self.scope.check(operation)
    }

    /// Moves the immutable joined peer and nonce to the owned networking handoff.
    /// Keep the originating FriendJoin alive, drive it through play, and explicitly close it.
    pub fn into_parts(mut self) -> (Uuid, String) {
        (self.peer, std::mem::take(&mut *self.nonce))
    }
}

/// Observable result of one explicitly driven admission phase.
pub enum JoinProgress {
    /// The host has supplied this joined player's nonce and a supported connection.
    Ready(FriendAdmission),
    /// MPSD explicitly rejected a concurrent write; the same intent may be retried after this instant.
    RetryAt(Instant),
}

#[derive(Clone)]
struct Scope {
    generation: String,
    xuid: String,
}

impl Scope {
    /// Captures only the current verified application identity, without credential duplication.
    fn from_operation(operation: &Operation) -> io::Result<Self> {
        let cached = operation
            .cached
            .as_ref()
            .ok_or_else(|| invalid("friend account is unavailable"))?;
        let identity = cached
            .identity
            .as_ref()
            .filter(|identity| identity.current_credential)
            .ok_or_else(|| invalid("friend account is not verified"))?;
        Ok(Self {
            generation: cached.generation.clone(),
            xuid: identity.xuid.clone(),
        })
    }

    /// Rejects account replacement even when the user signs into the same Xbox identity again.
    fn check(&self, operation: &Operation) -> io::Result<()> {
        let current = Self::from_operation(operation)?;
        if current.generation != self.generation || current.xuid != self.xuid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "friend-world account generation changed",
            ));
        }
        Ok(())
    }

    /// Supplies RTA's bounded generation identity; exact original generation equality is checked separately.
    fn rta_generation(&self) -> io::Result<rta::Generation> {
        let digest = Sha256::digest(self.generation.as_bytes());
        let value = u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 prefix"));
        rta::Generation::new(value.max(1)).map_err(|_| invalid("invalid friend account generation"))
    }
}

/// A single explicitly selected join whose membership remains reachable across canceled futures.
/// No task, runtime or reconnect policy is created. Call advance to admit, drive through play,
/// and close on every termination path; dropping this value performs no remote I/O.
#[must_use = "retain the friend-world owner and explicitly close its remote membership"]
pub struct FriendJoin {
    account: Arc<NativeAccount>,
    selection: FriendSelection,
    scope: Option<Scope>,
    socket: Option<RtaSocket>,
    subscription: Option<rta::SubscriptionId>,
    subscribed: bool,
    session_subscription: Uuid,
    binding: Option<mpsd::Binding>,
    attempt: Option<mpsd::JoinAttempt>,
    session: Option<Box<mpsd::Session>>,
    ambiguous: bool,
    changed: bool,
    retry_at: Option<Instant>,
    deadline: Instant,
    admitted: bool,
    prepared: bool,
    admission_identity: Option<(Uuid, [u8; 32])>,
    closing: bool,
    closed: bool,
}

impl NativeAccount {
    /// Creates a caller-owned intent; no authentication, subscription or membership I/O starts here.
    pub fn friend_join(
        self: Arc<Self>,
        selection: FriendSelection,
        deadline: Instant,
    ) -> io::Result<FriendJoin> {
        check_deadline(deadline)?;
        Ok(FriendJoin {
            account: self,
            selection,
            scope: None,
            socket: None,
            subscription: None,
            subscribed: false,
            session_subscription: Uuid::new_v4(),
            binding: None,
            attempt: None,
            session: None,
            ambiguous: false,
            changed: false,
            retry_at: None,
            deadline,
            admitted: false,
            prepared: false,
            admission_identity: None,
            closing: false,
            closed: false,
        })
    }
}

impl FriendJoin {
    /// Prepares this joined world's signed transport on the same verified leased account.
    /// Retain and drive this owner alongside the returned connection, then explicitly close it.
    /// Matching updates during preparation retire the handoff and require an explicit retry.
    pub async fn prepare_friend(
        &mut self,
        claims: serde_json::Value,
        random_id: protocol::ClientRandomId,
    ) -> io::Result<(
        protocol::NativeConnectionTarget,
        protocol::NativeAuthentication,
    )> {
        if !self.admitted || self.prepared || self.closing || self.closed {
            return Err(invalid("friend membership is not active"));
        }
        if !claims.is_object() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native client data must be a JSON object",
            ));
        }
        let mut operation = self.operation(self.deadline).await?;
        while self.changed {
            self.refresh_owned(&mut operation, self.deadline).await?;
        }
        let admission = self.current_admission()?;
        admission.validate(&operation)?;
        let (peer, nonce) = admission.into_parts();
        let target = protocol::NativeMessagingTarget::new(peer, nonce)?;
        let account = Arc::clone(&self.account);
        let prepared = service(
            self.socket
                .as_mut()
                .ok_or_else(|| invalid("friend RTA is unavailable"))?,
            self.subscription
                .ok_or_else(|| invalid("friend subscription is unavailable"))?,
            &self.selection.reference,
            &mut self.changed,
            self.deadline,
            account.prepare_messaging_borrowed(&mut operation, target, claims, random_id),
        )
        .await??;
        if self.changed {
            // Its protected socket needs continuous driving; retire it before awaiting another service read.
            drop(prepared);
            while self.changed {
                self.refresh_owned(&mut operation, self.deadline).await?;
            }
            self.current_admission()?.validate(&operation)?;
            operation.persist()?;
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "friend-world metadata changed during preparation; retry",
            ));
        }
        check_deadline(self.deadline)?;
        operation.persist()?;
        check_deadline(self.deadline)?;
        self.prepared = true;
        Ok(prepared)
    }

    /// Establishes active RTA, joins once, then keeps control traffic progressing until host admission.
    /// Cancellation keeps the stable join intent, known session and ambiguity fence in this owner.
    /// A canceled/failed ambiguous PUT is reconciled by the selected reference, never blindly replayed.
    pub async fn advance(&mut self) -> io::Result<JoinProgress> {
        if self.closed || self.closing || self.admitted {
            return Err(invalid("friend join is no longer admitting"));
        }
        check_deadline(self.deadline)?;
        self.update_ambiguity();
        self.connect().await?;
        if let Some(retry_at) = self.retry_at {
            if Instant::now() < retry_at {
                return Ok(JoinProgress::RetryAt(retry_at));
            }
            self.retry_at = None;
        }
        if self.ambiguous && self.session.is_none() {
            self.reconcile(self.deadline).await?;
        }
        if self.session.is_none() {
            let mut operation = self.operation(self.deadline).await?;
            let socket = self
                .socket
                .as_mut()
                .ok_or_else(|| invalid("friend RTA is unavailable"))?;
            let subscription = self
                .subscription
                .ok_or_else(|| invalid("friend subscription is unavailable"))?;
            let binding = socket
                .connection()
                .directory_binding(subscription, self.session_subscription)
                .map_err(rta_error)?;
            self.binding = Some(binding);
            if self.attempt.is_none() {
                self.attempt = Some(
                    operation
                        .minecraft()?
                        .mpsd()
                        .join_attempt(self.selection.handle, binding, mpsd::MemberData::default())
                        .map_err(directory_error)?,
                );
            }
            self.ambiguous = true;
            let result = service(
                socket,
                subscription,
                &self.selection.reference,
                &mut self.changed,
                self.deadline,
                operation
                    .minecraft()?
                    .mpsd()
                    .join(self.attempt.as_mut().expect("prepared join")),
            )
            .await?;
            match result.map_err(directory_error)? {
                mpsd::JoinOutcome::Joined(session) => {
                    self.session = Some(session);
                    self.ambiguous = false;
                }
                mpsd::JoinOutcome::Precondition { retry_after, .. } => {
                    self.ambiguous = false;
                    let retry_at = Instant::now()
                        .checked_add(retry_after)
                        .ok_or_else(|| invalid("friend retry deadline overflow"))?;
                    self.retry_at = Some(retry_at);
                    operation.persist()?;
                    return Ok(JoinProgress::RetryAt(retry_at));
                }
                _ => {
                    self.ambiguous = false;
                    return Err(invalid("friend-world join was rejected"));
                }
            }
            operation.persist()?;
        }
        loop {
            if self.changed {
                self.refresh(self.deadline).await?;
                continue;
            }
            let session = self
                .session
                .as_ref()
                .ok_or_else(|| invalid("friend membership is unavailable"))?;
            if !session.reference().same(&self.selection.reference)
                || session.is_closed()
                || session.needs_sync()
            {
                return Err(invalid("joined friend-world reference is unavailable"));
            }
            self.check_binding(session)?;
            if let Some(admission) = world::admission(
                session.description(),
                self.scope.as_ref().expect("connected scope"),
                &self.selection.host,
            )? {
                let mut operation = self.operation(self.deadline).await?;
                admission.validate(&operation)?;
                operation.persist()?;
                if self.changed {
                    continue;
                }
                check_deadline(self.deadline)?;
                self.admission_identity = Some((
                    admission.peer,
                    Sha256::digest(admission.nonce.as_bytes()).into(),
                ));
                self.admitted = true;
                return Ok(JoinProgress::Ready(admission));
            }
            self.notification(self.deadline).await?;
        }
    }

    /// Drives the active subscription and refreshes complete snapshots after matching shoulder taps.
    /// The transport owner polls this alongside game I/O; it must not spawn a detached lifetime task.
    pub async fn drive(&mut self, deadline: Instant) -> io::Result<()> {
        if !self.admitted || self.closing || self.closed {
            return Err(invalid("friend membership is not active"));
        }
        if !self.changed {
            self.notification(deadline).await?;
        }
        while self.changed {
            self.refresh(deadline).await?;
        }
        self.current_admission()?;
        Ok(())
    }

    /// Reconciles an uncertain write and explicitly leaves only this account's membership.
    /// Cancellation keeps cleanup reachable; call close again with a fresh finite cleanup deadline.
    pub async fn close(&mut self, deadline: Instant) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        check_deadline(deadline)?;
        self.update_ambiguity();
        self.closing = true;
        if self.ambiguous && self.session.is_none() {
            self.reconcile(deadline).await?;
        }
        if self
            .session
            .as_ref()
            .is_some_and(|session| !session.is_closed())
        {
            let mut operation = self.operation(deadline).await?;
            let session = self.session.as_mut().expect("checked membership");
            if session.needs_sync() || self.changed {
                bounded(deadline, operation.minecraft()?.mpsd().sync(session))
                    .await?
                    .map_err(directory_error)?;
                self.changed = false;
            }
            if !session.is_closed() {
                let binding = self
                    .binding
                    .ok_or_else(|| invalid("friend cleanup binding is unavailable"))?;
                match bounded(
                    deadline,
                    operation.minecraft()?.mpsd().leave_bound(session, binding),
                )
                .await?
                .map_err(directory_error)?
                {
                    mpsd::Update::Closed => {}
                    _ => {
                        return Err(invalid(
                            "friend membership cleanup requires synchronization",
                        ));
                    }
                }
            }
            operation.persist()?;
        }
        if let Some(socket) = &mut self.socket {
            socket.abort();
        }
        self.session = None;
        self.attempt = None;
        self.closed = true;
        Ok(())
    }

    /// Acquires the sole existing account/lease owner and fences every service operation by generation.
    async fn operation(&mut self, deadline: Instant) -> io::Result<Operation> {
        let acquire = Arc::clone(&self.account).acquire_verified();
        let mut operation = if !self.closing {
            if let (Some(socket), Some(subscription)) = (&mut self.socket, self.subscription) {
                service(
                    socket,
                    subscription,
                    &self.selection.reference,
                    &mut self.changed,
                    deadline,
                    acquire,
                )
                .await??
            } else {
                bounded(deadline, acquire).await??
            }
        } else {
            bounded(deadline, acquire).await??
        };
        if let Some(scope) = &self.scope {
            scope.check(&operation)?;
        }
        operation.promote()?;
        if let Some(socket) = &self.socket
            && !operation.minecraft()?.owns_rta(socket.connection())
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "friend-world proof owner changed",
            ));
        }
        if !self.closing
            && let Some(expires) = self.selection.expires
            && operation.minecraft()?.service_time() >= expires
        {
            return Err(invalid("friend-world invitation expired"));
        }
        Ok(operation)
    }

    /// Installs the actual protected RTA socket before awaiting the directory subscription result.
    async fn connect(&mut self) -> io::Result<()> {
        if self.socket.is_none() {
            let mut operation = self.operation(self.deadline).await?;
            let scope = Scope::from_operation(&operation)?;
            if scope.xuid == self.selection.host
                || self
                    .selection
                    .recipient
                    .as_deref()
                    .is_some_and(|recipient| recipient != scope.xuid)
            {
                return Err(invalid("friend-world recipient does not match the account"));
            }
            let headers = rta::websocket::upgrade_headers().map_err(socket_error)?;
            let mut request = bounded(
                self.deadline,
                operation
                    .minecraft()?
                    .rta_connection(scope.rta_generation()?, headers),
            )
            .await?
            .map_err(rta_error)?;
            let options = Options {
                connect_timeout: self.deadline.saturating_duration_since(Instant::now()),
                ..Options::default()
            };
            let result = bounded(self.deadline, RtaSocket::connect(&mut request, options)).await?;
            if matches!(result, Err(SocketError::UpgradeStatus(401))) {
                operation.minecraft()?.reject_rta(&request);
            }
            self.socket = Some(result.map_err(socket_error)?);
            self.scope = Some(scope);
            operation.persist()?;
        }
        if self.subscription.is_none() {
            let (subscription, _) = self
                .socket
                .as_mut()
                .expect("connected socket")
                .subscribe(rta::Resource::session_directory(), self.deadline)
                .map_err(rta_error)?;
            self.subscription = Some(subscription);
        }
        while !self.subscribed {
            self.notification(self.deadline).await?;
        }
        Ok(())
    }

    /// Recovers possibly applied membership without repeating an ambiguous handle PUT.
    async fn reconcile(&mut self, deadline: Instant) -> io::Result<()> {
        let mut operation = self.operation(deadline).await?;
        let mut directory = operation.minecraft()?.mpsd();
        let read = directory.read(&self.selection.reference);
        let result = if self.closing {
            bounded(deadline, read).await?
        } else {
            service(
                self.socket
                    .as_mut()
                    .ok_or_else(|| invalid("friend RTA is unavailable"))?,
                self.subscription
                    .ok_or_else(|| invalid("friend subscription is unavailable"))?,
                &self.selection.reference,
                &mut self.changed,
                deadline,
                read,
            )
            .await?
        };
        match result {
            Ok(session) => {
                if !session.is_bound(
                    self.binding
                        .ok_or_else(|| invalid("friend join binding is unavailable"))?,
                ) {
                    return Err(cleanup_pending());
                }
                self.session = Some(Box::new(session));
                self.ambiguous = false;
            }
            // A read cannot prove an earlier canceled write will never apply later.
            Err(mpsd::Error::Status(404)) => return Err(cleanup_pending()),
            Err(error) => return Err(directory_error(error)),
        }
        operation.persist()?;
        Ok(())
    }

    /// Refreshes, rather than merging, title metadata while keeping subscription controls progressing.
    async fn refresh(&mut self, deadline: Instant) -> io::Result<()> {
        let mut operation = self.operation(deadline).await?;
        self.refresh_owned(&mut operation, deadline).await?;
        operation.persist()?;
        Ok(())
    }

    /// Refreshes under an already held lease so preparation never reacquires its own account owner.
    async fn refresh_owned(
        &mut self,
        operation: &mut Operation,
        deadline: Instant,
    ) -> io::Result<()> {
        self.scope
            .as_ref()
            .ok_or_else(|| invalid("friend account scope is unavailable"))?
            .check(operation)?;
        // Preserve the dirty fence if this future is canceled before the complete snapshot arrives.
        self.changed = true;
        let mut changed = false;
        let socket = self
            .socket
            .as_mut()
            .ok_or_else(|| invalid("friend RTA is unavailable"))?;
        let subscription = self
            .subscription
            .ok_or_else(|| invalid("friend subscription is unavailable"))?;
        service(
            socket,
            subscription,
            &self.selection.reference,
            &mut changed,
            deadline,
            operation.minecraft()?.mpsd().sync(
                self.session
                    .as_mut()
                    .ok_or_else(|| invalid("friend membership is unavailable"))?,
            ),
        )
        .await?
        .map_err(directory_error)?;
        self.changed = changed;
        Ok(())
    }

    /// Consumes one ordered control event without creating a polling task or accumulating payloads.
    async fn notification(&mut self, deadline: Instant) -> io::Result<()> {
        let event = bounded(
            deadline,
            self.socket
                .as_mut()
                .ok_or_else(|| invalid("friend RTA is unavailable"))?
                .drive(),
        )
        .await?
        .map_err(socket_error)?;
        if let rta::Event::Subscribed { subscription, .. } = &event
            && Some(*subscription) == self.subscription
        {
            self.subscribed = true;
        }
        event_changed(
            event,
            self.subscription
                .ok_or_else(|| invalid("friend subscription is unavailable"))?,
            &self.selection.reference,
            &mut self.changed,
        )
    }

    /// Requires the joined local membership to reflect this socket's actual acknowledged RTA binding.
    fn check_binding(&self, session: &mpsd::Session) -> io::Result<()> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| invalid("friend RTA is unavailable"))?;
        let binding = socket
            .connection()
            .directory_binding(
                self.subscription
                    .ok_or_else(|| invalid("friend subscription is unavailable"))?,
                self.session_subscription,
            )
            .map_err(rta_error)?;
        if !session.is_bound(binding) {
            return Err(invalid(
                "friend membership is not bound to the active subscription",
            ));
        }
        Ok(())
    }

    /// Revalidates the retained member/peer/nonce identity before mint, handoff or continued play.
    fn current_admission(&self) -> io::Result<FriendAdmission> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| invalid("friend membership is unavailable"))?;
        self.check_binding(session)?;
        let admission = world::admission(
            session.description(),
            self.scope
                .as_ref()
                .ok_or_else(|| invalid("friend account scope is unavailable"))?,
            &self.selection.host,
        )?
        .ok_or_else(|| invalid("friend-world admission was revoked"))?;
        if self.admission_identity
            != Some((
                admission.peer,
                Sha256::digest(admission.nonce.as_bytes()).into(),
            ))
        {
            return Err(invalid("friend-world admission identity changed"));
        }
        Ok(admission)
    }

    /// Distinguishes cancellation during credential acquisition from an actually dispatched join write.
    fn update_ambiguity(&mut self) {
        if self.ambiguous {
            self.ambiguous = self
                .attempt
                .as_ref()
                .is_some_and(mpsd::JoinAttempt::needs_reconciliation);
        }
    }
}

/// Polls RTA controls concurrently with one borrowed service operation, preserving its owner on cancellation.
async fn service<T>(
    socket: &mut RtaSocket,
    subscription: rta::SubscriptionId,
    reference: &mpsd::Reference,
    changed: &mut bool,
    deadline: Instant,
    future: impl Future<Output = T>,
) -> io::Result<T> {
    check_deadline(deadline)?;
    tokio::pin!(future);
    let timeout = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
    tokio::pin!(timeout);
    poll_fn(|cx| {
        if Instant::now() >= deadline || timeout.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(timed_out()));
        }
        let event = {
            let drive = socket.drive();
            tokio::pin!(drive);
            drive.as_mut().poll(cx)
        };
        let progressed = match event {
            Poll::Ready(Ok(event)) => {
                if let Err(error) = event_changed(event, subscription, reference, changed) {
                    return Poll::Ready(Err(error));
                }
                true
            }
            Poll::Ready(Err(error)) => return Poll::Ready(Err(socket_error(error))),
            Poll::Pending => false,
        };
        if let Poll::Ready(value) = future.as_mut().poll(cx) {
            return Poll::Ready(Ok(value));
        }
        if progressed {
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    })
    .await
}

/// Handles only this directory subscription's revision hints; payload snapshots remain HTTP-owned.
fn event_changed(
    event: rta::Event,
    subscription: rta::SubscriptionId,
    reference: &mpsd::Reference,
    changed: &mut bool,
) -> io::Result<()> {
    match event {
        rta::Event::SessionsChanged {
            subscription: received,
            changes,
        } if received == subscription => {
            *changed |= changes.iter().any(|change| change.is_for(reference))
        }
        rta::Event::Resync { subscriptions } => *changed |= subscriptions.contains(&subscription),
        rta::Event::Failed { .. } | rta::Event::Disconnected | rta::Event::Unsubscribed { .. } => {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "friend-world subscription ended",
            ));
        }
        _ => {}
    }
    Ok(())
}

/// Gives the original absolute deadline priority before polling a possibly ready I/O future.
async fn bounded<T>(deadline: Instant, future: impl Future<Output = T>) -> io::Result<T> {
    check_deadline(deadline)?;
    tokio::select! { biased;
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Err(timed_out()),
        value = future => Ok(value),
    }
}

/// Rejects expired phases before starting account or network I/O.
fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(timed_out())
    } else {
        Ok(())
    }
}

/// Keeps service bodies, handles and title metadata out of diagnostics.
fn directory_error(_: mpsd::Error) -> io::Error {
    io::Error::other("friend-world directory request failed")
}
/// Keeps protected upgrade details out of diagnostics.
fn rta_error(_: rta::Error) -> io::Error {
    io::Error::other("friend-world subscription failed")
}
/// Keeps WebSocket responses and close reasons out of diagnostics.
fn socket_error(_: SocketError) -> io::Error {
    io::Error::other("friend-world realtime connection failed")
}
/// Reports only application-owned static validation reasons.
fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}
/// Reports the retained deadline rather than exposing transport diagnostics.
fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "friend-world operation timed out")
}

/// Retains ambiguous membership instead of claiming that a canceled write could not have applied.
fn cleanup_pending() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "friend-world membership remains uncertain; retry cleanup",
    )
}

#[cfg(test)]
mod tests;
