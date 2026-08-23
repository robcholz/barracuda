#![allow(clippy::expect_used)]
#![allow(clippy::panic)]
#![allow(missing_docs)]

use barracuda_event_router::{RpcLaneStorage, RpcRegistry, RpcStream};
use barracuda_vm_component::VmLimits;
use barracuda_vm_component::run::{ChunkBoundary, Run, RunErrorKind, RunRequestFrame};
use futures_lite::{future::block_on, stream};
use zerocopy::TryFromBytes;

#[derive(Debug, Eq, PartialEq)]
enum Outcome {
    Output(String),
    Error(RunErrorKind),
}

fn run(limits: VmLimits, requests: Vec<RunRequestFrame>) -> Vec<Outcome> {
    block_on(async move {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 64, 2>::new()));
        let registry = RpcRegistry::new(lanes);
        let _registration = registry
            .register::<Run, _>(barracuda_vm_component::run::run_handler(limits))
            .expect("register vm.run");
        let input = RpcStream::new(stream::iter(requests.into_iter().map(Ok)));
        let mut output = registry.client().call::<Run>(input).expect("start vm.run");
        let mut outcomes = Vec::new();
        while let Some(item) = output.next().await {
            match item.expect("RPC transport") {
                Ok(frame) => outcomes.push(Outcome::Output(
                    frame
                        .view()
                        .expect("response frame")
                        .text()
                        .expect("response UTF-8")
                        .to_owned(),
                )),
                Err(error) => outcomes.push(Outcome::Error(
                    error.view().expect("method error frame").kind(),
                )),
            }
        }
        outcomes
    })
}

fn source(text: &str) -> RunRequestFrame {
    RunRequestFrame::source(text, ChunkBoundary::Complete).expect("source frame")
}

fn malformed_text_frame(kind: u8) -> RunRequestFrame {
    let mut bytes = [0_u8; 64];
    bytes[0] = kind;
    bytes[1] = 1;
    bytes[2] = b'a';
    bytes[3] = 0;
    bytes[4] = b'b';
    *RunRequestFrame::try_ref_from_bytes(&bytes).expect("valid fixed layout")
}

#[test]
fn request_eof_closes_lua_input() {
    assert_eq!(
        run(
            VmLimits::default(),
            vec![source("local i=require'io'; i.print(i.input() == nil)")],
        ),
        vec![Outcome::Output("true".to_owned())]
    );
}

#[test]
fn request_eof_commits_a_partial_input_message() {
    let partial = RunRequestFrame::input("partial", ChunkBoundary::More).expect("input frame");
    assert_eq!(
        run(
            VmLimits::default(),
            vec![source("local i=require'io'; i.print(i.input())"), partial],
        ),
        vec![Outcome::Output("partial".to_owned())]
    );
}

#[test]
fn protocol_and_allocation_limits_are_method_errors() {
    let premature_input =
        RunRequestFrame::input("hello", ChunkBoundary::Complete).expect("input frame");
    assert_eq!(
        run(VmLimits::default(), vec![premature_input],),
        vec![Outcome::Error(RunErrorKind::InvalidProtocol)]
    );
    assert_eq!(
        run(VmLimits::new(3, 16), vec![source("return")],),
        vec![Outcome::Error(RunErrorKind::SourceLimitExceeded)]
    );
    let oversized_input =
        RunRequestFrame::input("four", ChunkBoundary::Complete).expect("input frame");
    assert_eq!(
        run(
            VmLimits::new(64, 3),
            vec![
                source("local i=require'io'; i.print(i.input())"),
                oversized_input
            ],
        ),
        vec![Outcome::Error(RunErrorKind::InputLimitExceeded)]
    );

    let unfinished_source =
        RunRequestFrame::source("return", ChunkBoundary::More).expect("source frame");
    assert_eq!(
        run(VmLimits::default(), vec![unfinished_source],),
        vec![Outcome::Error(RunErrorKind::InvalidProtocol)]
    );

    assert_eq!(
        run(
            VmLimits::default(),
            vec![source("local i=require'io'; i.input()"), source("return")],
        ),
        vec![Outcome::Error(RunErrorKind::InvalidProtocol)]
    );

    assert_eq!(
        run(VmLimits::default(), vec![malformed_text_frame(0)],),
        vec![Outcome::Error(RunErrorKind::InvalidText)]
    );
    assert_eq!(
        run(
            VmLimits::default(),
            vec![
                source("local i=require'io'; i.input()"),
                malformed_text_frame(1)
            ],
        ),
        vec![Outcome::Error(RunErrorKind::InvalidText)]
    );
}

#[test]
fn lua_output_precedes_the_terminal_runtime_error() {
    assert_eq!(
        run(
            VmLimits::default(),
            vec![source(
                "local i=require'io'; i.print('before'); error('boom')"
            )],
        ),
        vec![
            Outcome::Output("before".to_owned()),
            Outcome::Error(RunErrorKind::LuaRuntime),
        ]
    );
}

#[test]
fn load_and_runtime_failures_are_distinct() {
    assert_eq!(
        run(VmLimits::default(), vec![source("this is not lua ???")],),
        vec![Outcome::Error(RunErrorKind::LuaLoad)]
    );
    assert_eq!(
        run(
            VmLimits::default(),
            vec![source(
                "local i=require'io'; i.print(string.char(97, 0, 98))"
            )],
        ),
        vec![Outcome::Error(RunErrorKind::LuaRuntime)]
    );
}

#[test]
fn output_can_arrive_while_execution_waits_for_input() {
    let input = RunRequestFrame::input("continue", ChunkBoundary::Complete).expect("input frame");
    assert_eq!(
        run(
            VmLimits::default(),
            vec![
                source("local i=require'io'; i.print('first'); i.input(); i.print()"),
                input
            ],
        ),
        vec![
            Outcome::Output("first".to_owned()),
            Outcome::Output(String::new()),
        ]
    );
}

#[test]
fn execution_can_finish_while_a_complete_or_eof_input_is_being_sent() {
    let complete = RunRequestFrame::input("ignored", ChunkBoundary::Complete).expect("input frame");
    assert_eq!(
        run(VmLimits::default(), vec![source("return"), complete],),
        Vec::new()
    );

    let partial = RunRequestFrame::input("ignored", ChunkBoundary::More).expect("input frame");
    assert_eq!(
        run(VmLimits::default(), vec![source("return"), partial],),
        Vec::new()
    );
}
