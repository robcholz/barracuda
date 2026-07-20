//! End-to-end use of `claw-persistence` with one singleton and one collection.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p claw-persistence --example basic
//! ```
//!
//! `MemFs` keeps the example hermetic. A production caller supplies its own
//! `ClawFs` implementation without changing the persistence API.

use std::{borrow::Cow, error::Error};

use claw_interface::MemFs;
use claw_persistence::{
    DurablePartError, DurableState, DurableStateCodec, InstanceId, Persistence, SchemaVersion,
    StateBlob, StateSlice,
};
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
    MemFs::new();

    let root = "/example";
    let session_id = InstanceId::new("session-1")?;

    let persistence = Persistence::<MemFs>::new(root)?;
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
    persistence.maybe_persist()?;

    drop(runtime);
    drop(session);
    drop(persistence);

    // Typed entries are reopened when the process starts again. Loading returns
    // only the decoded DTO; a runtime owner creates its own DurableState.
    let resumed = Persistence::<MemFs>::new(root)?;
    let runtime_entry = resumed.singleton::<ExampleState>("runtime")?;
    let sessions_entry = resumed.collection::<ExampleState>("sessions")?;

    let runtime = runtime_entry.load()?.expect("runtime state was persisted");
    let session = sessions_entry
        .load(&session_id)?
        .expect("session state was persisted");

    println!("runtime turns: {}", runtime.turn_count);
    println!("session turns: {}", session.turn_count);
    println!("persisted sessions: {:?}", sessions_entry.list()?);

    sessions_entry.remove(&session_id)?;
    Ok(())
}
