//! Application-level logging integration tests.

#![allow(clippy::expect_used)]

use std::rc::Rc;
use std::sync::Mutex;

use futures_lite::future::block_on;
use log::{LevelFilter, Log, Metadata, Record};
use web::{InboundError, InboundFuture, InboundMessage, InboundMessageSink, WebService};

static CAPTURE: CaptureLogger = CaptureLogger {
    records: Mutex::new(Vec::new()),
};

struct CaptureLogger {
    records: Mutex<Vec<String>>,
}

impl Log for CaptureLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target().starts_with("web")
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            self.records
                .lock()
                .expect("lock captured records")
                .push(format!("{} {}", record.level(), record.args()));
        }
    }

    fn flush(&self) {}
}

struct Sink {
    reject: bool,
}

impl InboundMessageSink for Sink {
    fn receive_message(&self, _request: InboundMessage) -> InboundFuture<'_, ()> {
        let reject = self.reject;
        Box::pin(async move {
            if reject {
                Err(InboundError::Rejected {
                    message: String::from("gateway unavailable"),
                })
            } else {
                Ok(())
            }
        })
    }
}

#[test]
fn web_service_logs_application_outcomes_without_message_content() {
    log::set_logger(&CAPTURE).expect("install capture logger");
    log::set_max_level(LevelFilter::Trace);

    block_on(async {
        let accepting = WebService::new(Rc::new(Sink { reject: false }));
        accepting
            .receive_message_json(
                "chat-42",
                br#"{"message_id":"client-1","thread_id":null,"text":"private body"}"#,
            )
            .await
            .expect("accept inbound message");

        let malformed = accepting.receive_message_json("chat-42", b"{").await;
        assert!(matches!(malformed, Err(InboundError::InvalidJson { .. })));

        let rejecting = WebService::new(Rc::new(Sink { reject: true }));
        let rejected = rejecting
            .receive_message_json(
                "chat-42",
                br#"{"message_id":"client-2","thread_id":null,"text":"another secret"}"#,
            )
            .await;
        assert!(matches!(rejected, Err(InboundError::Rejected { .. })));
    });

    let records = CAPTURE.records.lock().expect("lock captured records");
    assert!(records.iter().any(|line| {
        line == "INFO IMessage Web accepted message `client-1` for conversation `chat-42`"
    }));
    assert!(records.iter().any(|line| {
        line.starts_with("WARN IMessage Web rejected malformed message for conversation `chat-42`")
    }));
    assert!(records.iter().any(|line| {
        line.starts_with(
            "WARN IMessage Web failed to deliver message `client-2` for conversation `chat-42`",
        )
    }));
    assert!(records
        .iter()
        .all(|line| !line.contains("private body") && !line.contains("another secret")));
}
