//! End-to-end use of `barracuda-agent-persistence` with one singleton and one collection.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p barracuda-agent-persistence --example basic
//! ```
//!
//! A mounted in-memory VFS keeps the example hermetic. Production callers pass
//! the plugin-private VFS namespace supplied by the system.

use std::{borrow::Cow, error::Error};

use barracuda_agent_persistence::{
    DurablePartError, DurableState, DurableStateCodec, InstanceId, Persistence, SchemaVersion,
    StateBlob, StateSlice,
};
use barracuda_platform_test::memory_vfs;
use futures_lite::future::block_on;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
struct ExampleState {
    name: String,
    turn_count: u32,
}

impl DurableStateCodec for ExampleState {
    const SCHEMA_VERSION: SchemaVersion = 1;

    fn encode_state(&self) -> Result<StateBlob<'_>, DurablePartError> {
        // JSON is this caller's codec choice. Persistence only sees opaque bytes.
        let bytes = serde_json::to_vec(self).map_err(DurablePartError::encode)?;
        Ok(StateBlob {
            bytes: Cow::Owned(bytes),
        })
    }

    fn decode_state(
        schema_version: SchemaVersion,
        state: StateSlice<'_>,
    ) -> Result<Self, DurablePartError> {
        if schema_version != Self::SCHEMA_VERSION {
            return Err(DurablePartError::InvalidState(
                "unsupported example state schema",
            ));
        }

        serde_json::from_slice(state.bytes).map_err(DurablePartError::decode)
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    block_on(run())
}

async fn run() -> Result<(), Box<dyn Error>> {
    let filesystem = memory_vfs().await?;

    let root = "/example";
    let session_id = InstanceId::new("session-1")?;

    let persistence = Persistence::new(filesystem.clone(), root).await?;
    let runtime_entry = persistence.singleton::<ExampleState>("runtime")?;
    let sessions_entry = persistence.collection::<ExampleState>("sessions")?;

    let runtime = DurableState::new(ExampleState {
        name: "runtime".to_owned(),
        turn_count: 0,
    });
    let session = DurableState::new(ExampleState {
        name: "first session".to_owned(),
        turn_count: 0,
    });
    runtime_entry.register(&runtime)?;
    sessions_entry.register(&session_id, &session)?;

    runtime.get_mut().turn_count += 1;
    session.get_mut().turn_count += 2;

    // Persist every dirty state captured above.
    persistence.maybe_persist().await?;

    drop(runtime);
    drop(session);
    drop(persistence);

    // Typed entries are reopened when the process starts again. Loading returns
    // only the decoded DTO; a runtime owner creates its own DurableState.
    let resumed = Persistence::new(filesystem, root).await?;
    let runtime_entry = resumed.singleton::<ExampleState>("runtime")?;
    let sessions_entry = resumed.collection::<ExampleState>("sessions")?;

    let runtime = runtime_entry
        .load()
        .await?
        .expect("runtime state was persisted");
    let session = sessions_entry
        .load(&session_id)
        .await?
        .expect("session state was persisted");

    println!("runtime turns: {}", runtime.turn_count);
    println!("session turns: {}", session.turn_count);
    println!("persisted sessions: {:?}", sessions_entry.list().await?);

    sessions_entry.remove(&session_id)?;
    Ok(())
}
