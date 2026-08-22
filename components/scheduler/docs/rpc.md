# Scheduler RPC API

The Scheduler Component stores bounded in-memory schedules. Its public mutation
RPCs are runtime-dynamic. `scheduler.schedule` itself makes a nested typed call
to `time.now` to validate the requested trigger against the authoritative RTC;
callers never provide the current time or a Unix timestamp. Schedules are not
persisted across Component or process restart.

Schedule IDs contain 1–31 ASCII letters, digits, `_`, `-`, or `.`.

## `scheduler.schedule`

- Address: `scheduler.schedule`
- Dynamic JSON: yes
- Request: unary `ScheduleRequest`
- Response: unary `ScheduleResponse`
- Method error: `ScheduleError`

Request fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | `ScheduleId` string | Stable caller-selected identity. |
| `trigger` | `Trigger` | One-time or fixed-interval trigger rule. |

`Trigger` is tagged by `type`:

- `once`: contains only `at`, the absolute UTC calendar time to trigger.
- `interval`: contains `at`, `every_seconds`, and `count`. `at` is the first
  trigger time and `count` is the total number of triggers including the first.

Both variants deliberately use the field name `at`; `starts_at` is not accepted.

Response fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | `ScheduleId` string | Accepted identity. |

Method errors:

| Variant | Meaning |
| --- | --- |
| `InvalidSchedule` | The `at` calendar fields or trigger-rule fields are invalid. |
| `DuplicateId` | A live schedule already owns `id`. |
| `CapacityExceeded` | The configured live-schedule capacity is full. |
| `NotFound` | Reserved for mutation operations requiring an existing schedule. |
| `TimeUnavailable` | The internal typed `time.now` call failed or returned invalid UTC fields. |
| `TriggerInPast` | The requested `at` is earlier than the current RTC value. |

One-time example:

```json
{
  "id": "meeting",
  "trigger": {
    "type": "once",
    "at": {
      "year": 2027,
      "month": 1,
      "day": 15,
      "hour": 8,
      "minute": 30,
      "second": 0
    }
  }
}
```

Interval example:

```json
{
  "id": "drink-water",
  "trigger": {
    "type": "interval",
    "at": {
      "year": 2027,
      "month": 1,
      "day": 15,
      "hour": 8,
      "minute": 30,
      "second": 0
    },
    "every_seconds": 3600,
    "count": 3
  }
}
```

## `scheduler.cancel`

- Address: `scheduler.cancel`
- Dynamic JSON: yes
- Request: unary `CancelRequest`
- Response: unary `CancelResponse`
- Method error: `ScheduleError`

Request fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | `ScheduleId` string | Live schedule to remove. |

Response fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | `ScheduleId` string | Cancelled identity. |
| `completed_runs` | `u64` | Events committed before cancellation. |

`NotFound` is returned when no live schedule owns `id`. If cancellation races
with Event emission, an Event already accepted by Event Router cannot be
recalled; cancellation prevents later occurrences.
