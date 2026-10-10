//! `imessage_bridge.session`: a conversation's session commands, from any channel.
//!
//! A route owns the sessions it started. `/sessions`, `/new`, `/switch`,
//! `/rename` and `/delete` (or the Web page's structured equivalents) list and
//! change them; the answer goes back to the conversation through
//! `IMessageGateway::send_sessions`, which text channels render as a message.

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
use core::{future::Future, pin::Pin};

use barracuda_imessage_gateway_plugin::{
    GatewayControlKind, GatewayInboundControl, MessageTarget, SendSessionsRequest, SessionEntry,
    SessionNotice,
};
use barracuda_plugin::manager::PluginStorage;
use barracuda_workflow_plugin::{
    WorkflowActionFuture, WorkflowActionHandler, WorkflowActionSchema, workflow_action_schema,
};
use serde::Serialize;

use crate::{
    bridge::{BridgeControl, BridgeShared},
    state::{BridgeError, Mapping, Route, forget_mapping, persist_mapping},
};

/// Runtime-neutral future used by the session seams.
pub(crate) type LocalFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// One Agent session's human-facing metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionMeta {
    pub(crate) session: String,
    pub(crate) title: Option<String>,
    pub(crate) updated_at: Option<u64>,
}

/// Why a rename did not happen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RenameFailure {
    InvalidTitle,
    NotFound,
    Failed,
}

/// The Agent sessions the bridge lists, renames and deletes.
pub(crate) trait SessionStore {
    /// Every live session.
    fn describe(&self) -> LocalFuture<'_, Vec<SessionMeta>>;
    fn rename<'a>(
        &'a self,
        session: &'a str,
        title: &'a str,
    ) -> LocalFuture<'a, Result<(), RenameFailure>>;
    /// Deletes a session; one already gone counts as deleted.
    fn delete<'a>(&'a self, session: &'a str) -> LocalFuture<'a, Result<(), ()>>;
}

/// Where a session command's answer goes, and the device's clock.
pub(crate) trait SessionReplies {
    fn send(&self, request: SendSessionsRequest) -> LocalFuture<'_, ()>;
    /// Unix milliseconds, or `None` while the clock is not synchronized.
    fn now(&self) -> Option<u64>;
}

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum SessionResponse {
    Done {},
    Error { error: BridgeError },
}

pub(crate) struct SessionAction<Storage> {
    control: BridgeControl<Storage>,
    store: Rc<dyn SessionStore>,
    replies: Rc<dyn SessionReplies>,
}

impl<Storage> SessionAction<Storage> {
    pub(crate) fn new(
        control: BridgeControl<Storage>,
        store: Rc<dyn SessionStore>,
        replies: Rc<dyn SessionReplies>,
    ) -> Self {
        Self {
            control,
            store,
            replies,
        }
    }
}

impl<Storage> WorkflowActionHandler for SessionAction<Storage>
where
    Storage: PluginStorage,
{
    type Request = GatewayInboundControl;
    type Response = SessionResponse;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("imessage_bridge.session");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let shared = Rc::clone(&self.control.shared);
        let store = Rc::clone(&self.store);
        let replies = Rc::clone(&self.replies);
        Box::pin(async move {
            Ok(match run(&shared, &*store, &*replies, request).await {
                Ok(()) => SessionResponse::Done {},
                Err(error) => {
                    log::warn!(
                        "IMessage Bridge rejected a session command: {}",
                        error.code()
                    );
                    SessionResponse::Error { error }
                }
            })
        })
    }
}

/// Carries out one session command and answers the conversation.
pub(crate) async fn run<Storage: PluginStorage>(
    shared: &BridgeShared<Storage>,
    store: &dyn SessionStore,
    replies: &dyn SessionReplies,
    request: GatewayInboundControl,
) -> Result<(), BridgeError> {
    let route = Route::new(
        &request.route.channel,
        &request.route.conversation_id,
        request.route.thread_id.as_deref(),
    )?;
    let metas = store.describe().await;
    forget_vanished(shared, &route, &metas).await;
    let list = listed(shared, &route, &metas).await;
    let picked = pick(&list, &request);
    let notice = match request.control {
        GatewayControlKind::Sessions => None,
        GatewayControlKind::New => {
            leave(shared, store, &route, request.temporary).await?;
            Some(SessionNotice::Created {
                temporary: request.temporary,
            })
        }
        GatewayControlKind::Switch => match picked {
            None => Some(SessionNotice::UnknownSession),
            Some((_, entry)) => {
                let already = shared
                    .book
                    .lock()
                    .await
                    .current_of(&route)
                    .is_some_and(|current| current.session() == entry.session);
                if !already {
                    leave(shared, store, &route, false).await?;
                    let mut book = shared.book.lock().await;
                    let mapping = book.make_current(&route, &entry.session)?;
                    persist_mapping(&shared.storage, &mapping).await?;
                    drop(book);
                    shared.routes.notify();
                    log::info!(
                        "IMessage Bridge switched `{}` conversation `{}` to `{}`",
                        route.channel,
                        route.conversation_id,
                        entry.session
                    );
                }
                Some(SessionNotice::Switched { title: entry.title })
            }
        },
        GatewayControlKind::Rename => match (picked, request.title.as_deref()) {
            (None, _) => Some(SessionNotice::UnknownSession),
            (Some(_), None) => Some(SessionNotice::InvalidTitle),
            (Some((_, entry)), Some(title)) => {
                Some(match store.rename(&entry.session, title).await {
                    Ok(()) => SessionNotice::Renamed {
                        title: store
                            .describe()
                            .await
                            .into_iter()
                            .find(|meta| meta.session == entry.session)
                            .and_then(|meta| meta.title)
                            .unwrap_or_else(|| String::from(title)),
                    },
                    Err(RenameFailure::InvalidTitle) => SessionNotice::InvalidTitle,
                    Err(RenameFailure::NotFound) => SessionNotice::UnknownSession,
                    Err(RenameFailure::Failed) => SessionNotice::Failed,
                })
            }
        },
        GatewayControlKind::Delete => match picked {
            None => Some(SessionNotice::UnknownSession),
            Some((index, entry)) if !request.confirm => Some(SessionNotice::ConfirmDelete {
                index: u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1),
                title: entry.title,
            }),
            Some((_, entry)) => {
                if store.delete(&entry.session).await.is_err() {
                    Some(SessionNotice::Failed)
                } else {
                    let removed = shared.book.lock().await.remove(&entry.session);
                    if removed.is_some() {
                        forget_mapping(&shared.storage, &entry.session).await?;
                    }
                    shared.routes.notify();
                    log::info!("IMessage Bridge deleted `{}`", entry.session);
                    Some(SessionNotice::Deleted { title: entry.title })
                }
            }
        },
        GatewayControlKind::Help | GatewayControlKind::Interrupt | GatewayControlKind::Cancel => {
            Some(SessionNotice::Usage)
        }
    };
    let metas = store.describe().await;
    let sessions = listed(shared, &route, &metas).await;
    let book = shared.book.lock().await;
    let current = book.current_of(&route);
    let reply = SendSessionsRequest {
        target: MessageTarget {
            channel: route.channel.clone(),
            conversation_id: route.conversation_id.clone(),
            thread_id: route.thread_id.clone(),
        },
        current: current.map(|mapping| String::from(mapping.session())),
        temporary: current.map_or_else(|| book.temporary_next(&route), Mapping::temporary),
        sessions,
        notice,
        now: replies.now(),
    };
    drop(book);
    replies.send(reply).await;
    Ok(())
}

/// Leaves the route's current session: its next message starts one. A
/// temporary session is deleted on the way out.
async fn leave<Storage: PluginStorage>(
    shared: &BridgeShared<Storage>,
    store: &dyn SessionStore,
    route: &Route,
    temporary: bool,
) -> Result<(), BridgeError> {
    let left = shared.book.lock().await.leave(route, temporary);
    shared.routes.notify();
    match left {
        Some(mapping) if mapping.temporary() => {
            log::info!("IMessage Bridge deleting temporary `{}`", mapping.session());
            let _deleted = store.delete(mapping.session()).await;
        }
        Some(mapping) => persist_mapping(&shared.storage, &mapping).await?,
        None => {}
    }
    Ok(())
}

/// Forgets the route's sessions the Agent no longer has (deleted elsewhere, or
/// temporary ones lost to a restart).
async fn forget_vanished<Storage: PluginStorage>(
    shared: &BridgeShared<Storage>,
    route: &Route,
    metas: &[SessionMeta],
) {
    let mut book = shared.book.lock().await;
    let gone: Vec<String> = book
        .sessions_of(route)
        .filter(|mapping| !metas.iter().any(|meta| meta.session == mapping.session()))
        .map(|mapping| String::from(mapping.session()))
        .collect();
    for session in gone {
        log::info!("IMessage Bridge forgetting `{session}`, which the Agent no longer has");
        book.remove(&session);
        let _forgotten = forget_mapping(&shared.storage, &session).await;
    }
}

/// The route's saved sessions, newest first: by last use, then by id.
async fn listed<Storage: PluginStorage>(
    shared: &BridgeShared<Storage>,
    route: &Route,
    metas: &[SessionMeta],
) -> Vec<SessionEntry> {
    let book = shared.book.lock().await;
    let mut entries: Vec<SessionEntry> = book
        .sessions_of(route)
        .filter(|mapping| !mapping.temporary())
        .filter_map(|mapping| {
            let meta = metas
                .iter()
                .find(|meta| meta.session == mapping.session())?;
            Some(SessionEntry {
                session: meta.session.clone(),
                title: meta.title.clone(),
                updated_at: meta.updated_at,
                running: mapping.running(),
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| number(&b.session).cmp(&number(&a.session)))
    });
    entries
}

fn number(session: &str) -> u64 {
    session
        .strip_prefix("session-")
        .and_then(|number| number.parse().ok())
        .unwrap_or(0)
}

/// The session a command names: by id, or by its 1-based place in the list.
fn pick(list: &[SessionEntry], request: &GatewayInboundControl) -> Option<(usize, SessionEntry)> {
    let index = match (&request.session, request.index) {
        (Some(session), _) => list.iter().position(|entry| entry.session == *session)?,
        (None, Some(number)) => usize::try_from(number).ok()?.checked_sub(1)?,
        (None, None) => return None,
    };
    list.get(index).map(|entry| (index, entry.clone()))
}
