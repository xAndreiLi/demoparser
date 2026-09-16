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
  sampleEveryTicks?: number
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

### Cadence `tick_sample`

A `{ event: "tick_sample", sampleEveryTicks: N, playerProps }` spec is a synthetic cadence clock inside the same scoped pass. It is not a second native function.

```ts
const events = parseEventsScoped(path, [
  {
    event: 'tick_sample',
    sampleEveryTicks: 8,
    playerProps: ['X', 'Y', 'Z', 'yaw', 'is_alive', 'team_num'],
  },
  {
    event: 'weapon_fire',
    playerProps: ['X', 'Y'],
  },
])
```

Behavior:

- rows are emitted after entity decode on non-fullpacket packets when `tick.rem_euclid(N) === 0`
- `includeAllPlayers` is implied; every row has `all_players` keyed by steamid with the requested player props
- `tick_sample` must be present as a spec so the Node filter keeps the rows
- adding a cadence spec does not change other event rows
- alignment is absolute (`tick % N == 0` via Euclidean remainder), not window-relative

The CPU win versus `parseTicks` is that cadence snapshots share the one `parseEventsScoped` second pass.

### Sidecar: `parseEventsScopedWithTracks`

At 8 Hz with every connected player, `tick_sample` produces 10k–25k rows per demo, each carrying a
nested `all_players` map. Building those rows, serialising them with `serde_json`, and converting
the resulting `Value` to JS objects roughly doubles the wall time of the scoped call, while the
entity decode itself is unchanged (it always applies every packet; cadence only gates readout).

`parseEventsScopedWithTracks` runs the same single pass but reads the cadence out as flat columns:

```ts
const { events, tracks } = parseEventsScopedWithTracks(path, [
  { event: 'player_death', playerProps: ['X', 'Y'] },
  { event: 'tick_sample', sampleEveryTicks: 8 },
])

// events: identical to parseEventsScoped(...) minus the tick_sample rows
// tracks: struct of typed arrays, one row per (tick, connected player), grouped by ascending tick
tracks.tick     // Int32Array
tracks.steamid  // BigUint64Array  (String(tracks.steamid[i]) for a steam64 string)
tracks.x, tracks.y, tracks.z, tracks.yaw  // Float32Array, NaN when missing
tracks.isAlive  // Uint8Array, 0/1, 255 when missing
tracks.teamNum  // Uint8Array, 255 when missing
```

Rules:

- exactly one `tick_sample` spec with `sampleEveryTicks > 0` is required (the call throws otherwise)
- the sidecar schema is fixed (`X Y Z yaw is_alive team_num`); those props are added to the parse
  automatically, so the `tick_sample` spec's `playerProps` may be omitted
- no `tick_sample` rows are emitted; `all_players` on other events is unaffected
- multithreaded segments are concatenated in file order; a tick sampled on both sides of a
  fullpacket boundary is kept once

Measured on five 170–440 MB demos (WSL2, 20 threads, median of 3): the plain scoped call with
`tick_sample` at 8 Hz was 1.7–1.9× the same specs without `tick_sample`; with the sidecar it was
0.98–1.08×, with identical positions.

Core: `TrackColumns` / `collect_track_sample` in `second_pass/game_events.rs`, enabled through
`Parser::with_track_sidecar(true)`.

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

Use `tick_sample` when:

- you need a regular pose clock (for example 8 Hz at `sampleEveryTicks: 8` on a 64-tick demo)
- you want those snapshots in the same `parseEventsScoped` call as game events

## Internal note

The core implementation now lives in:

- `src/parser/src/parse_demo.rs`
- `src/parser/src/second_pass/parser_settings.rs`
- `src/parser/src/second_pass/game_events.rs`
- `src/node/src/lib.rs`

The Node binding is now a thin translator for scoped config rather than the layer that performs an extra parser pass.
