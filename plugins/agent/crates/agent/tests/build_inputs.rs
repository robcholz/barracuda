//! Regression coverage for Agent manifest build-script inputs.

use std::path::Path;

#[path = "../manifest_gen/inputs.rs"]
#[allow(dead_code)]
mod inputs;

#[test]
fn common_manifest_watch_inputs_exist() {
    let common = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/agents/common");

    for relative in inputs::COMMON_FILES {
        let input = common.join(relative);
        assert!(
            input.is_file(),
            "build script watches missing common manifest input {}",
            input.display()
        );
    }
}
