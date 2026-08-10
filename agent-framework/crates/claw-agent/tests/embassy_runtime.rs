use claw_agent::{AgentPersistenceConfig, AgentSystem, SessionPersistence};
use claw_interface::http::{
    Cancel, ClawHttp, HttpError, HttpJsonRequest, HttpResponseFuture, HttpStatusCode, SliceChunks,
    StreamingHttp,
};
use claw_interface::{ImmediateTimer, MemFs};
use futures_lite::future::{block_on, zip};

#[derive(Default)]
struct NullHttp;

impl ClawHttp for NullHttp {
    fn post_json<'a>(
        &'a mut self,
        _request: &'a HttpJsonRequest<'a>,
        _cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async { Err(HttpError::Aborted) })
    }
}

impl StreamingHttp for NullHttp {
    type ByteStream<'a> = SliceChunks<'a>;

    async fn post_json_streaming<'a, 'r>(
        &'a mut self,
        _request: &'r HttpJsonRequest<'r>,
        _cancel: Cancel<'a>,
    ) -> Result<(HttpStatusCode, Self::ByteStream<'a>), HttpError> {
        Err(HttpError::Aborted)
    }
}

#[test]
fn executor_neutral_service_drives_public_session_api() {
    let (system, service) = AgentSystem::<MemFs, NullHttp, ImmediateTimer>::new(
        MemFs::new(),
        AgentPersistenceConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
    )
    .expect("runtime builds");

    block_on(async move {
        let scenario = async {
            let session = system
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            assert_eq!(system.list_sessions().await, vec![session]);
            system
                .delete_session(session)
                .await
                .expect("session deletes");
            assert!(system.list_sessions().await.is_empty());
            system.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}
