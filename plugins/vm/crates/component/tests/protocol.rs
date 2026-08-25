#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use barracuda_vm_component::run::{ChunkBoundary, FrameTextError, RunRequestFrame, RunRequestKind};

#[test]
fn request_frames_are_bounded_nul_terminated_utf8_exposed_as_json_text() {
    let frame = RunRequestFrame::source("return '你好'", ChunkBoundary::Complete)
        .expect("valid source frame");
    assert_eq!(frame.kind(), RunRequestKind::Source);
    assert_eq!(frame.text(), Ok("return '你好'"));

    let json = serde_json::to_string(&frame).expect("serialize request frame");
    assert!(json.contains("\"text\":\"return '你好'\""));
    let decoded: RunRequestFrame = serde_json::from_str(&json).expect("deserialize request frame");
    assert_eq!(decoded, frame);

    assert_eq!(
        RunRequestFrame::input(&"x".repeat(62), ChunkBoundary::Complete),
        Err(FrameTextError::TooLong)
    );
    assert_eq!(
        RunRequestFrame::input("before\0after", ChunkBoundary::Complete),
        Err(FrameTextError::ContainsNul)
    );
}
