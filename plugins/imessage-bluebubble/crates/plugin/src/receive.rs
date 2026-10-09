//! The receive session: registers the webhook, catches up, then handles
//! webhook deliveries queued by the endpoint until receiving stops.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;

use barracuda_imessage_gateway_channel::{
    ChannelControl, Classification, ReceiveChannel, ReceiveError, ReceiveFuture, ReceiveSession,
    PAIRED_REPLY,
};
use barracuda_imessage_gateway_plugin::{
    ChannelError, GatewayInboundMessage, GatewayRoute, MessageChannel, MessageTarget,
    SendMessageRequest,
};
use barracuda_plugin::manager::PluginStorage;
use bluebubbles::inbound::{is_body_too_large, InboundMessage, MessageQuery, Sort};
use bluebubbles::{BlueBubbles, CHANNEL};
use embassy_futures::select::{select3, Either3};
use embassy_time::{Duration, Instant, Timer};
use http_client::embedded_nal_async::{Dns, TcpConnect};

use crate::channel::BlueBubblesChannel;
use crate::state::{SecretError, HOOK_PATH};

/// How often a healthy session checks that its webhook is still registered.
pub(crate) const RECHECK_INTERVAL: Duration = Duration::from_secs(180);
/// Shortest time between two cursor writes.
#[cfg(not(test))]
pub(crate) const FLUSH_INTERVAL: Duration = Duration::from_secs(10);
#[cfg(test)]
pub(crate) const FLUSH_INTERVAL: Duration = Duration::from_millis(20);
/// Messages one catch-up handles at most; older ones are skipped and counted.
pub(crate) const CATCH_UP_MAX: u64 = 50;
/// Messages asked for per catch-up page; halved while a page is too large.
pub(crate) const CATCH_UP_PAGE: u32 = 10;
/// Wait before retrying after the server rate-limited the device.
const RATE_LIMIT_RETRY: Duration = Duration::from_secs(30);

/// The channel's [`ReceiveChannel`].
pub(crate) struct Receiver<Storage, T: 'static, D: 'static>(
    pub(crate) Rc<BlueBubblesChannel<Storage, T, D>>,
);

impl<Storage, T, D> ReceiveChannel<()> for Receiver<Storage, T, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    fn receive<'a>(&'a self, _lease: &'a mut (), session: ReceiveSession<'a>) -> ReceiveFuture<'a> {
        Box::pin(self.0.session(session))
    }
}

impl<Storage, T, D> BlueBubblesChannel<Storage, T, D>
where
    Storage: PluginStorage,
    T: TcpConnect + 'static,
    D: Dns + 'static,
{
    /// One receive session: set up the webhook, catch up, then serve the
    /// inbox and re-check the registration until the runtime drops it.
    async fn session(&self, session: ReceiveSession<'_>) -> Result<(), ReceiveError> {
        let Some(provider) = self.provider() else {
            return Err(ReceiveError::halt("BlueBubbles is not configured"));
        };
        let hook = self
            .book
            .secret_for(&self.storage, &self.entropy, provider.server_url())
            .await
            .map_err(|error| match error {
                SecretError::Entropy => ReceiveError::halt("no entropy for the webhook secret"),
                SecretError::Storage(error) => ReceiveError::new(format!("storage: {error}")),
            })?;
        let Some(url) = self.hook_url(&hook.secret) else {
            return Err(ReceiveError::new("device LAN address unknown"));
        };
        if self.book.after().is_none() {
            self.start_cursor(&provider).await?;
        }
        self.register_webhook(&provider, &url).await?;
        session.receiving();
        self.catch_up(&provider).await?;
        let mut recheck = Instant::now().saturating_add(RECHECK_INTERVAL);
        let mut last_flush = Instant::MIN;
        loop {
            let flush_at = last_flush.saturating_add(FLUSH_INTERVAL);
            let dirty = self.book.is_dirty();
            let flush_due = async move {
                if dirty {
                    Timer::at(flush_at).await;
                } else {
                    core::future::pending::<()>().await;
                }
            };
            match select3(self.book.arrival(), Timer::at(recheck), flush_due).await {
                Either3::First(()) => {
                    if self.book.take_missed() {
                        self.catch_up(&provider).await?;
                    }
                    if self.book.has_queued() {
                        self.inbound.ready().await;
                        if let Some(message) = self.book.next() {
                            self.handle(&provider, message).await;
                        }
                    }
                }
                Either3::Second(()) => {
                    // The server never retries a webhook delivery, and the
                    // webserver refuses bodies over its buffer, so the
                    // re-check also catches up whatever was missed.
                    self.register_webhook(&provider, &url).await?;
                    self.catch_up(&provider).await?;
                    recheck = Instant::now().saturating_add(RECHECK_INTERVAL);
                }
                Either3::Third(()) => {
                    last_flush = Instant::now();
                    if let Err(error) = self.book.flush(&self.storage).await {
                        log::warn!("failed to store the BlueBubbles cursor: {error}");
                    }
                }
            }
        }
    }

    /// Starts a new cursor at the newest message, so a new channel does not
    /// replay the server's history.
    async fn start_cursor(
        &self,
        provider: &BlueBubbles<'static, T, D>,
    ) -> Result<(), ReceiveError> {
        let page = provider
            .query_messages(MessageQuery {
                after: None,
                offset: 0,
                limit: 1,
                sort: Sort::Descending,
            })
            .await
            .map_err(receive_error)?;
        match page.messages.first() {
            Some(newest) if newest.date_created.is_some() => self.book.record(newest),
            _ => self.book.start_at(0),
        }
        Ok(())
    }

    /// Makes sure `url` is registered, deleting this device's stale entries
    /// (an old secret or address).
    async fn register_webhook(
        &self,
        provider: &BlueBubbles<'static, T, D>,
        url: &str,
    ) -> Result<(), ReceiveError> {
        let Some(mut hook) = self.book.hook() else {
            return Err(ReceiveError::new("webhook secret missing"));
        };
        let suffix = format!("{HOOK_PATH}/{}", hook.secret);
        let mut present = false;
        for entry in provider.list_webhooks().await.map_err(receive_error)? {
            if entry.url == url {
                present = true;
            } else if hook.url.as_deref() == Some(entry.url.as_str())
                || entry.url.ends_with(&suffix)
            {
                log::info!("deleting a stale BlueBubbles webhook");
                provider
                    .delete_webhook(entry.id)
                    .await
                    .map_err(receive_error)?;
            }
        }
        if !present {
            provider.create_webhook(url).await.map_err(receive_error)?;
            log::info!("registered the BlueBubbles webhook");
        }
        if hook.url.as_deref() != Some(url) {
            hook.url = Some(String::from(url));
            self.book
                .store_hook(&self.storage, hook)
                .await
                .map_err(|error| ReceiveError::new(format!("storage: {error}")))?;
        }
        Ok(())
    }

    /// Handles messages created since the cursor, oldest first: at most
    /// [`CATCH_UP_MAX`], skipping (and counting) older ones beyond that.
    async fn catch_up(&self, provider: &BlueBubbles<'static, T, D>) -> Result<(), ReceiveError> {
        let after = self.book.after();
        let mut offset = 0_u64;
        let mut limit = CATCH_UP_PAGE;
        let mut handled = 0_u64;
        let mut first = true;
        while handled < CATCH_UP_MAX {
            self.inbound.ready().await;
            let query = MessageQuery {
                after,
                offset,
                limit,
                sort: Sort::Ascending,
            };
            let page = match provider.query_messages(query).await {
                Ok(page) => page,
                Err(error) if is_body_too_large(&error) && limit > 1 => {
                    limit = limit.div_ceil(2);
                    continue;
                }
                Err(error) if is_body_too_large(&error) => {
                    log::warn!("skipped a BlueBubbles message too large to read");
                    self.book.lose_many(1);
                    offset = offset.saturating_add(1);
                    handled = handled.saturating_add(1);
                    continue;
                }
                Err(error) => return Err(receive_error(error)),
            };
            if core::mem::take(&mut first) {
                if let Some(skip) = page
                    .total
                    .and_then(|total| total.checked_sub(CATCH_UP_MAX))
                    .filter(|skip| *skip > 0)
                {
                    log::warn!("skipping {skip} BlueBubbles messages older than the catch-up");
                    self.book.lose_many(u32::try_from(skip).unwrap_or(u32::MAX));
                    offset = skip;
                    continue;
                }
            }
            let count = page.messages.len();
            for message in page.messages {
                if !self.book.is_seen(&message.guid) {
                    self.handle(provider, message).await;
                }
            }
            offset = offset.saturating_add(count as u64);
            handled = handled.saturating_add(count as u64);
            if count < usize::try_from(limit).unwrap_or(usize::MAX) {
                break;
            }
        }
        Ok(())
    }

    /// Records `message`, then publishes it, answers a pairing, or drops it
    /// by its sender.
    async fn handle(&self, provider: &BlueBubbles<'static, T, D>, message: InboundMessage) {
        self.book.record(&message);
        if message.is_from_me {
            return;
        }
        let Some(text) = message.plain_text() else {
            log::debug!("skipped a BlueBubbles message that is not plain text");
            self.book.skip();
            return;
        };
        let (Some(sender), Some(chat)) = (message.sender(), message.chat_guid()) else {
            log::debug!("skipped a BlueBubbles message without sender or chat");
            self.book.skip();
            return;
        };
        // A receiving channel is configured, so its owner book is loaded.
        let Some(owners) = self.owners() else {
            return;
        };
        match owners.classify(sender, None, text).await {
            Classification::Owner => {
                let inbound = GatewayInboundMessage {
                    route: GatewayRoute::new(CHANNEL, chat),
                    message_id: message.guid.clone(),
                    text: String::from(text),
                };
                if let Err(error) = self.inbound.publish(inbound).await {
                    log::warn!("failed to publish a BlueBubbles message: {error}");
                }
            }
            Classification::Paired => {
                let reply =
                    SendMessageRequest::text(MessageTarget::new(CHANNEL, chat), PAIRED_REPLY);
                if let Err(error) = provider.send_message(reply).await {
                    log::warn!("failed to answer a BlueBubbles pairing: {error}");
                }
            }
            Classification::Ignored => {}
        }
    }
}

/// Maps a server failure onto the runtime's retry policy.
fn receive_error(error: ChannelError) -> ReceiveError {
    match error {
        ChannelError::Authentication => ReceiveError::halt("BlueBubbles rejected the password"),
        ChannelError::RateLimited => {
            ReceiveError::new("BlueBubbles rate limit").retry_after(RATE_LIMIT_RETRY)
        }
        other => ReceiveError::new(format!("BlueBubbles: {other}")),
    }
}
