# Scheduler JSON Events

## `scheduler.triggered`

- Event ID: `scheduler.triggered`
- Topic: the schedule's `id`
- Emission: once after the `UtcClock` reports that a live occurrence is due

The Event input is a JSON object:

```json
{"id":"morning","run_number":1}
```

`id` is the schedule identity and `run_number` is the one-based count of
accepted occurrences. Scheduler emits it directly to `WorkflowService` as a
`serde_json::Value`.

The occurrence is committed only after Workflow accepts the Event. The
affected schedule key is then atomically updated or deleted. A one-shot or
exhausted finite schedule is removed. For interval schedules,
missed intervals coalesce: one overdue Event is emitted, then the next deadline
advances to the first interval strictly after the observed UTC time. Skipped
slots do not consume `count`; only accepted Events do.

No Event is emitted while `UtcClock::now` reports an error.

Delivery across power loss is at-least-once. If power fails after Workflow
accepts an Event but before the affected schedule key commits, recovery may
emit that occurrence again. Workflow and Plugin KV do not share one
transaction.

A Workflow can match every occurrence by Event ID or select one schedule by
Topic:

```json
{
  "id": "morning-alarm",
  "match": {
    "event": "scheduler.triggered",
    "topic": "morning"
  },
  "steps": [{"call": "alarm.ring"}]
}
```
