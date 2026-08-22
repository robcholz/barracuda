# Scheduler Events

## `scheduler.triggered`

- Message type: `Triggered`
- Cardinality: unary
- Topic: the schedule's `id` as a fixed Event Topic
- Emission: once when the Component's typed `time.now` reading reports that a
  live schedule is due

Payload fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | `ScheduleId` | Schedule identity. |
| `run_number` | `u64` | One-based committed run number. |

The occurrence is committed only after Event Router accepts this Event. A
one-shot or exhausted finite schedule is then removed. For periodic schedules,
missed intervals coalesce: one overdue Event is emitted, then the next deadline
advances to the first interval strictly after the observed RTC time. Coalesced
slots do not consume `count`; only emitted Events do.

No Event is emitted while `time.now` reports an error.

The Event ID remains `scheduler.triggered` for every schedule. A Workflow can
subscribe to all scheduler occurrences with only `match.event`, or select a
schedule by adding `match.topic`:

```json
{
  "id": "morning-alarm",
  "match": {
    "event": "scheduler.triggered",
    "topic": "morning"
  },
  "steps": [{ "call": "alarm.ring" }]
}
```
