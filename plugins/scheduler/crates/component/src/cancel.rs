use alloc::rc::Rc;

use barracuda_event_router::{
    JsonHandler, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, json_schema,
};
use barracuda_plugin_manager::PluginStorage;
use serde::Deserialize;

use crate::{
    component::SchedulerControl,
    json::{ScheduleCancelled, ScheduleRejected},
    model::ScheduleId,
    schedule::ScheduleError,
};

/// Cancels one live schedule.
pub struct Cancel;

impl JsonRpcSchema for Cancel {
    const ADDRESS: &'static str = "scheduler.cancel";
    const REQUEST_SCHEMA: JsonSchema = json_schema!("cancel", request);
    const RESPONSE_SCHEMA: JsonSchema = json_schema!("cancel", response);
    const MAX_REQUEST_BYTES: usize = 512;
    const MAX_RESPONSE_BYTES: usize = 64;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelRequest<'a> {
    #[serde(borrow)]
    id: &'a str,
}

/// Builds the reusable JSON handler for [`Cancel`].
pub(crate) fn cancel_handler<Storage>(control: SchedulerControl<Storage>) -> impl JsonHandler
where
    Storage: PluginStorage,
{
    move |_context, request: JsonRef, response: JsonWriter| {
        let shared = Rc::clone(&control.shared);
        async move {
            let request = request.deserialize::<CancelRequest<'_>>()?;
            let result = match ScheduleId::new(request.id) {
                Ok(id) => {
                    let mut book = shared.book.lock().await;
                    match book.cancel(&id) {
                        Ok(cancelled) => {
                            if shared.delete(id).await.is_err() {
                                let _restored = book.restore_cancelled(cancelled);
                                Err(ScheduleError::StorageUnavailable)
                            } else {
                                Ok((id, cancelled))
                            }
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(_error) => Err(ScheduleError::InvalidSchedule),
            };

            match result {
                Ok((id, cancelled)) => {
                    shared.changed.signal(());
                    response
                        .write(&ScheduleCancelled::new(id, cancelled.completed_runs()))
                        .await
                }
                Err(error) => response.write(&ScheduleRejected::new(error)).await,
            }
        }
    }
}
