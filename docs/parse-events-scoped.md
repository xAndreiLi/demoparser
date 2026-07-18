# parseEventsScoped

`parseEventsScoped` is the event-oriented alternative to composing `parseEvents` and repeated `parseTicks` calls.

It is designed for workloads where you:

- care about a small set of event types
- want different prop scopes per event type
- may want per-event all-player snapshots
- want to avoid replaying the second pass multiple times

## Why this exists

The older composition patterns have different tradeoffs:

- `parseEvent` / `parseEvents` with extras: good event-centric output, but each call still runs a full parser pass
- `parseEvents` + `parseTicks`: flexible, but expensive because the second pass is replayed again for each scoped tick query

`parseEventsScoped` unions the requested event names and props and runs one parser pass.

## API shape

```ts
type JsVariant = boolean | string | number | bigint

interface ScopedEventFieldRef {
  field: string
}

type ScopedEventFilterValue = JsVariant | ScopedEventFieldRef

interface ScopedEventTickFilter {
  field: string
  op: 'eq' | 'neq' | 'in'
  value?: ScopedEventFilterValue
  values?: Array<ScopedEventFilterValue>
}

interface ScopedEventSpec {
  event: string
  playerProps?: Array<string>
  otherProps?: Array<string>
  where?: Record<string, JsVariant>
  includeAllPlayers?: boolean
  tickFilter?: ScopedEventTickFilter | Array<ScopedEventTickFilter>
}

function parseEventsScoped(
  pathOrBuf: string | Buffer,
  scopedEvents: Array<ScopedEventSpec>,
  gameEventListBytes?: Buffer | undefined | null,
): any
```

## Execution model

For each call:

1. The binding unions the requested event names, player props, and other props across all specs.
2. The parser runs one first pass and one second pass.
3. Raw game events are decoded.
4. A core `tickFilter` gate is evaluated against raw event fields.
5. If the tick filter passes, scoped extra props are materialized.
6. If `includeAllPlayers` is enabled and the tick filter passes, a nested `all_players` snapshot is attached using the live player state at that event tick.
7. The Node binding filters returned event fields so each event only exposes the props requested by the matching spec(s).

## `where` vs `tickFilter`

These are different tools.

### `where`

`where` is a simple equality matcher used to decide whether an event matches a scoped spec.

```ts
{
  event: 'player_hurt',
  where: { weapon: 'glock' }
}
```

Use it when you want to select only certain event instances.

### `tickFilter`

`tickFilter` is a core-backed gate for additional scoped enrichment.

If it fails:

- the event is still emitted
- scoped extra props are skipped
- `all_players` is skipped

Use it when you want the base event stream, but only want expensive extra state on selected event instances.

## `tickFilter` semantics

### Supported operators

- `eq`
- `neq`
- `in`

### Supported operands

A filter can compare a field to:

- a static value
- another event field

Static comparison example:

```ts
{
  event: 'weapon_fire',
  playerProps: ['X', 'Y'],
  tickFilter: {
    field: 'weapon',
    op: 'eq',
    value: 'weapon_flashbang'
  }
}
```

Field-to-field comparison example:

```ts
{
  event: 'player_hurt',
  playerProps: ['X'],
  tickFilter: {
    field: 'attacker',
    op: 'neq',
    value: { field: 'userid' }
  }
}
```

`in` example:

```ts
{
  event: 'weapon_fire',
  playerProps: ['X'],
  tickFilter: {
    field: 'weapon',
    op: 'in',
    values: ['weapon_flashbang', 'weapon_hegrenade']
  }
}
```

### Current limitation

`tickFilter` currently runs on raw event fields only.

That means it can reference fields like:

- `weapon`
- `userid`
- `attacker`
- `site`
- `tick`
- `event_name`

It cannot reference enriched scoped fields like:

- `user_X`
- `attacker_health`
- `game_time`
- `all_players`

## `includeAllPlayers`

When `includeAllPlayers` is enabled, matching events can include a nested per-player snapshot:

```ts
{
  event: 'player_hurt',
  playerProps: ['X'],
  includeAllPlayers: true
}
```

Example result shape:

```json
{
  "tick": 12345,
  "event_name": "player_hurt",
  "weapon": "glock",
  "user_X": 100,
  "all_players": {
    "76561198000000001": { "X": 100 },
    "76561198000000002": { "X": -250 }
  }
}
```

Notes:

- only requested player props are included inside each `all_players` entry
- the snapshot is built in the parser core during the same second pass
- this avoids the older extra `parseTicks(orderBySteamid=true)` pass

## Examples

### Different prop scopes per event

```ts
const events = parseEventsScoped(path, [
  {
    event: 'player_death',
    playerProps: ['X', 'Y', 'Z', 'health'],
    otherProps: ['game_time', 'total_rounds_played'],
  },
  {
    event: 'bomb_planted',
    playerProps: ['X', 'Y'],
    otherProps: ['game_time'],
  },
])
```

### Grenade-oriented `weapon_fire` query

```ts
const events = parseEventsScoped(path, [
  {
    event: 'weapon_fire',
    playerProps: ['X', 'Y', 'Z'],
    includeAllPlayers: true,
    tickFilter: {
      field: 'weapon',
      op: 'eq',
      value: 'weapon_flashbang',
    },
  },
])
```

This will keep the `weapon_fire` event stream, but only attach the extra player state for flashbang throws.

### Field-to-field filter

```ts
const events = parseEventsScoped(path, [
  {
    event: 'player_hurt',
    playerProps: ['X', 'health'],
    tickFilter: {
      field: 'attacker',
      op: 'neq',
      value: { field: 'userid' },
    },
  },
])
```

## Performance notes

`parseEventsScoped` is meant to replace multi-pass event/tick compositions.

High-level behavior:

- `parseEventsScoped` without `includeAllPlayers` is the cheapest scoped path
- `includeAllPlayers` is more expensive than local event-only enrichment, but still avoids a second parser pass
- payload size can grow substantially when `all_players` is attached to many events

The CPU win comes from keeping event enrichment and optional all-player snapshots inside one second pass.

## Practical guidance

Use `parseEventsScoped` when:

- different events need different prop scopes
- you want event-centric output
- you want to avoid repeated `parseTicks` passes

Use `where` when:

- you want to select matching event instances

Use `tickFilter` when:

- you still want the event instances, but only want expensive enrichment for some of them

Use `includeAllPlayers` when:

- local actor props are not enough
- you need whole-lobby state at matching event ticks
- the extra payload size is acceptable

## Internal note

The core implementation now lives in:

- `src/parser/src/parse_demo.rs`
- `src/parser/src/second_pass/parser_settings.rs`
- `src/parser/src/second_pass/game_events.rs`
- `src/node/src/lib.rs`

The Node binding is now a thin translator for scoped config rather than the layer that performs an extra parser pass.
