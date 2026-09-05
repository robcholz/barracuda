# Scheduler JSON Events

## `scheduler.triggered`

- Event ID: `scheduler.triggered`
- Topic: the schedule's `id`
- Maximum input document: 59 bytes
- Emission: once after the `UtcClock` reports that a live occurrence is due

The Event input is a bounded JSON object:

```json
{"id":"morning","run_number":1}
```

`id` is the schedule identity and `run_number` is the one-based count of
accepted occurrences. Scheduler writes this document directly into Event
Router storage from its fixed-capacity ID and counter; it does not construct a
`serde_json::Value` or intermediate `String`.

The occurrence is committed only after Event Router accepts the Event. The
affected schedule key is then atomically updated or deleted. A one-shot or
exhausted finite schedule is removed. For interval schedules,
missed intervals coalesce: one overdue Event is emitted, then the next deadline
advances to the first interval strictly after the observed UTC time. Skipped
slots do not consume `count`; only accepted Events do.

No Event is emitted while `UtcClock::now` reports an error. This is a sequence
of independent bounded Events, not native RPC streaming.

Delivery across power loss is at-least-once. If power fails after Event Router
accepts an Event but before the affected schedule key commits, recovery may
emit that occurrence again. Event Router and Plugin KV do not share one
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
