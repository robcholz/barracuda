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
    DurablePartError, DurableStateCodec, Entry, InstanceId, Persistence, SchemaVersion, StateBlob,
    StateSlice,
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
    let runtime_entry = Entry::singleton("runtime");
    let sessions_entry = Entry::collection("sessions");
    let session_id = InstanceId::new("session-1")?;

    let persistence = Persistence::<MemFs>::new(root)?;
    persistence.create_template::<ExampleState>(runtime_entry.clone())?;
    persistence.create_template::<ExampleState>(sessions_entry.clone())?;

    let runtime = persistence.put(
        &runtime_entry,
        None,
        ExampleState {
            name: "runtime".to_owned(),
            turn_count: 0,
        },
    )?;
    let session = persistence.put(
        &sessions_entry,
        Some(session_id.clone()),
        ExampleState {
            name: "first session".to_owned(),
            turn_count: 0,
        },
    )?;

    runtime.get_mut().turn_count += 1;
    session.get_mut().turn_count += 2;

    // Persist every dirty state captured above.
    persistence.maybe_persist()?;

    drop(runtime);
    drop(session);
    drop(persistence);

    // Templates contain runtime codec/type information, so they are recreated
    // when the process starts again. The state itself is restored lazily by get().
    let resumed = Persistence::<MemFs>::new(root)?;
    resumed.create_template::<ExampleState>(runtime_entry.clone())?;
    resumed.create_template::<ExampleState>(sessions_entry.clone())?;

    let runtime = resumed.get::<ExampleState>(&runtime_entry, None)?;
    let session = resumed.get::<ExampleState>(&sessions_entry, Some(&session_id))?;

    println!("runtime turns: {}", runtime.get().turn_count);
    println!("session turns: {}", session.get().turn_count);
    println!("persisted sessions: {:?}", resumed.list(&sessions_entry)?);

    resumed.remove(&sessions_entry, Some(&session_id))?;
    Ok(())
}
