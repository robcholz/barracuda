use crate::agent::InflightToolCall;
use crate::protocol::{
    InputRequestId, InputRequestIdAllocator, InputRequestKind, Message, TurnId, TurnIdAllocator,
    TurnOrigin,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingInputRequest {
    pub(super) id: InputRequestId,
    pub(super) kind: InputRequestKind,
}

pub(super) enum PendingTurnInput {
    Submit(Message),
    Response {
        kind: InputRequestKind,
        message: Message,
    },
}

struct ActiveTurn {
    id: TurnId,
    origin: TurnOrigin,
    pending_input: Option<PendingTurnInput>,
    input_request: Option<PendingInputRequest>,
    toolcalls: Vec<InflightToolCall>,
}

pub(super) struct FinishedTurn {
    pub(super) id: TurnId,
    pub(super) toolcalls: Vec<InflightToolCall>,
}

pub(crate) struct TurnState {
    active_turn: Option<ActiveTurn>,
    next_turn_id: TurnId,
    next_input_request_id: InputRequestId,
}

impl Default for TurnState {
    fn default() -> Self {
        Self {
            active_turn: None,
            next_turn_id: TurnIdAllocator::new().peek(),
            next_input_request_id: InputRequestIdAllocator::new().peek(),
        }
    }
}

impl TurnState {
    pub(super) fn has_active_turn(&self) -> bool {
        self.active_turn.is_some()
    }

    pub(super) fn active_turn_id(&self) -> Option<TurnId> {
        self.active_turn.as_ref().map(|turn| turn.id)
    }

    pub(super) fn active_turn_origin(&self) -> Option<TurnOrigin> {
        self.active_turn.as_ref().map(|turn| turn.origin)
    }

    pub(super) fn has_pending_input(&self) -> bool {
        self.active_turn
            .as_ref()
            .is_some_and(|turn| turn.pending_input.is_some())
    }

    pub(super) fn begin_user_turn(&mut self, input: Message) -> TurnId {
        self.begin_turn(
            TurnOrigin::User,
            Some(PendingTurnInput::Submit(input.into_user())),
        )
    }

    pub(super) fn begin_subagent_turn(&mut self, origin: TurnOrigin) -> TurnId {
        debug_assert!(matches!(origin, TurnOrigin::Subagent { .. }));
        self.begin_turn(origin, None)
    }

    fn begin_turn(
        &mut self,
        origin: TurnOrigin,
        pending_input: Option<PendingTurnInput>,
    ) -> TurnId {
        debug_assert!(self.active_turn.is_none());
        let id = self.next_turn_id;
        self.next_turn_id = TurnId::new(id.0.saturating_add(1));
        self.active_turn = Some(ActiveTurn {
            id,
            origin,
            pending_input,
            input_request: None,
            toolcalls: Vec::new(),
        });
        id
    }

    pub(super) fn request_input(
        &mut self,
        idle_origin: TurnOrigin,
        kind: InputRequestKind,
    ) -> Option<InputRequestId> {
        if self.has_pending_input() {
            return None;
        }
        if self.active_turn.is_none() {
            self.begin_subagent_turn(idle_origin);
        }
        let turn = self.active_turn.as_mut()?;
        if turn.input_request.is_some() {
            return None;
        }
        let id = self.next_input_request_id;
        self.next_input_request_id = InputRequestId::new(id.0.saturating_add(1));
        turn.input_request = Some(PendingInputRequest { id, kind });
        Some(id)
    }

    pub(super) fn active_input_request(&self) -> Option<&PendingInputRequest> {
        self.active_turn.as_ref()?.input_request.as_ref()
    }

    pub(super) fn cancel_input_request(&mut self) -> Option<InputRequestId> {
        self.active_turn
            .as_mut()?
            .input_request
            .take()
            .map(|request| request.id)
    }

    pub(super) fn respond_to_input(&mut self, request: InputRequestId, input: Message) -> bool {
        let Some(turn) = self.active_turn.as_mut() else {
            return false;
        };
        if turn.input_request.as_ref().map(|pending| pending.id) != Some(request) {
            return false;
        }
        let pending = turn
            .input_request
            .take()
            .expect("validated input request is present");
        turn.pending_input = Some(PendingTurnInput::Response {
            kind: pending.kind,
            message: input.into_user(),
        });
        true
    }

    pub(super) fn take_pending_input(&mut self) -> Option<PendingTurnInput> {
        self.active_turn.as_mut()?.pending_input.take()
    }

    pub(super) fn record_tool_started(&mut self, call: InflightToolCall) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.toolcalls.push(call);
        }
    }

    pub(super) fn finish_turn(&mut self) -> Option<FinishedTurn> {
        let turn = self.active_turn.take()?;
        Some(FinishedTurn {
            id: turn.id,
            toolcalls: turn.toolcalls,
        })
    }
}
