//! Selected Board/Platform resource-factory integration test.

#![allow(clippy::expect_used)]

use std::sync::mpsc::{sync_channel, SyncSender};
use std::time::Duration;

use embassy_executor::{Executor, Spawner};

#[embassy_executor::task]
async fn construct_resources(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
    let result = barracuda_target::resources(spawner)
        .await
        .map(|_resources| ())
        .map_err(|error| error.to_string());
    let _ignored = completed.send(result);
}

#[test]
fn selected_target_constructs_resources() {
    let (completed, result) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| {
            spawner
                .spawn(construct_resources(spawner, completed))
                .expect("spawn selected-target resource test");
        });
    });

    result
        .recv_timeout(Duration::from_secs(5))
        .expect("selected-target resource construction timed out")
        .expect("selected-target resource construction failed");
}
