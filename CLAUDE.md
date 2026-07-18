# Demoparser — Agent Development Notes

This file records key architectural insights and non-obvious implementation facts
for agents working on this repository in future sessions.

---

## Repository layout

```
src/
  parser/      # Core Rust parsing library (no bindings)
  node/        # NAPI-RS TypeScript bindings (builds .node addon)
  python/      # PyO3 Python bindings
  csgoproto/   # Protobuf code-gen crate (requires protoc on PATH to rebuild)
  wasm/        # WASM bindings (separate build)
```

The `parser` crate is the source of truth. The `node`, `python`, and `wasm`
crates are thin wrappers that expose the same underlying parser.

---

## Build

```bash
# Core library only (fast, no protoc needed):
cd src/parser && cargo check

# Node addon (requires yarn + Rust toolchain):
cd src/node && yarn build

# csgoproto regeneration (requires protoc):
cd src/csgoproto && cargo build
```

---

## Two-pass parsing architecture

Every call to `parseEvents` / `parseTicks` runs the full pipeline:

```
.dem bytes
  │
  ▼ FirstPassParser::parse_demo_setup_only()  ← scans entire file
      DemSendTables  → sendtable schema (Serializer tree)
      DemClassInfo   → entity class registry (cls_by_id, qf_mapper, prop_controller)
      DemSignonPacket→ game-event schema (ge_list), string tables, userinfo
      DemFullPacket  → records byte offsets + extracts baselines
      DemFileHeader  → demo metadata (map, tickrate, …)
  │
  ▼ FirstPassOutput  (borrows from FirstPassParser — see lifetime notes below)
  │
  ├─ Single-threaded → SecondPassParser::start(bytes)
  └─ Multi-threaded  → Rayon par_iter over fullpacket_offsets
                        each offset → independent SecondPassParser
                        results merged by combine_outputs()
  │
  ▼ DemoOutput
```

### First pass role
- Scans the **entire** file sequentially (expensive — O(file size))
- Builds the entity schema from `DemSendTables` + `DemClassInfo`
- Records all `DemFullPacket` byte offsets (used to parallelise the second pass)
- Extracts entity baselines (default entity state for each class)
- Parses the game-event descriptor list

### Second pass role
- Re-reads frames, this time decoding entity deltas and game events
- Uses the `cls_by_id` / `qf_mapper` from the first pass to decode fields
- `collect_entities()` is called each tick to harvest wanted prop values into `PropColumn`

---

## Lifetime model (`FirstPassOutput<'a>`)

`FirstPassOutput<'a>` contains **borrow references** into `FirstPassParser<'a>`:

```rust
pub struct FirstPassOutput<'a> {
    pub settings:        &'a ParserInputs<'a>,
    pub prop_controller: &'a PropController,
    pub cls_by_id:       &'a Vec<Class>,
    pub qfmap:           &'a QfMapper,
    pub ge_list:         &'a AHashMap<i32, DescriptorT>,
    // … plus owned data (baselines, string_tables, …)
}
```

**Critical rule**: `FirstPassParser` must outlive any `FirstPassOutput` it produced.
This means you **cannot** store `FirstPassOutput` in a cache or static — it only
lives for the duration of a single parse call stack.

To work around this (e.g. for caching), split the mutable loop from the output
creation:
```rust
// OK — mutable borrow ends when setup_only returns ()
first_pass_parser.parse_demo_setup_only(bytes, false)?;
// Now immutable borrows are fine
let cached = first_pass_parser.create_cached_structure(mtime);
let output = first_pass_parser.create_first_pass_output()?;
```

Calling `create_cached_structure` (immutable) AFTER `parse_demo` (which returns
a borrowing `FirstPassOutput`) will be rejected by the borrow checker because
Rust treats the mutable borrow as still live.

---

## Threading model

`check_multithreadability()` returns `false` for props that require cross-tick
state (e.g. velocity, which needs position deltas). When `false`, the second
pass runs single-threaded. This is checked against `ParserInputs.wanted_player_props`.

In multi-threaded mode each Rayon worker gets a **clone** of `FirstPassOutput`.
Cloning is cheap for the borrow fields but copies `baselines`, `string_tables`,
and `stringtable_players` — these are not large in practice.

---

## Settings-dependent vs settings-independent first-pass data

| Data | Settings-dependent? | Why |
|---|---|---|
| `fullpacket_offsets` | No | Byte positions in the file |
| `baselines` | No | Entity baseline bytes |
| `string_tables` / `stringtable_players` | No | Wire data |
| `ge_list` | No | Game-event descriptor |
| `header` | No | File metadata |
| `cls_by_id` | **YES** | `FieldInfo.should_parse` and `prop_id` are set based on `wanted_player_props` during `find_prop_name_paths` |
| `qf_mapper` | No (built alongside cls_by_id but from schema only) | |
| `prop_controller` | **YES** | Built from `wanted_player_props` / `wanted_other_props` |

---

## First-pass structure cache

Implemented to avoid re-running the expensive full-file scan when the same `.dem`
file is parsed multiple times in the same Node.js process (e.g. `parseEvents`
followed by `parseTicks`).

**Location**: `src/parser/src/parse_demo.rs` — `DEMO_CACHE` global +
`CachedFirstPassStructure` struct.

**Key**: canonical file path string. Validated by `mtime` comparison on each
access (stale entries are silently replaced).

**What is cached**: everything settings-independent — fullpacket_offsets,
baselines, string_tables, stringtable_players, header, ge_list — plus the raw
decompressed bytes of the `DemSendTables` and `DemClassInfo` frames.

**What is NOT cached**: `cls_by_id`, `prop_controller` (settings-dependent).
On a cache hit these are rebuilt by replaying `parse_sendtable_bytes` +
`parse_class_info` from the cached raw bytes. This is fast (no I/O, just
protobuf decode + field traversal).

**Cache does NOT apply** when bytes are passed as a `Buffer` (no stable path key).

---

## Huffman lookup table

`huffman_lookup_table()` in `second_pass/parser_settings.rs` returns a
`&'static Vec<(u8, u8)>` backed by a `OnceLock`. The table is derived entirely
from the embedded `huf.b` binary and is **identical for every demo file**.
It is initialised once per process and shared across all NAPI calls.

Do not call `create_huffman_lookup_table()` — that function no longer exists.

---

## PropController and prop IDs

Each prop name is assigned a stable `u32` ID during `parse_sendtable`.
These IDs are the keys used in `PropColumn` (the columnar output data).

Special IDs for synthetic props (tick, steamid, name, velocity, etc.) are
defined as public constants in `first_pass/prop_controller.rs` starting at
`NORMAL_PROP_BASEID = 1000`.

`prop_controller.find_prop_name_paths(&mut ser)` walks each `Serializer` and
sets `FieldInfo.should_parse = true` and assigns a `prop_id` for every field
the caller requested. Fields with `should_parse = false` are decoded minimally
(bit-advanced but value discarded) in the second pass for performance.

---

## Entity model (second pass)

Entities are stored as `Vec<Option<Entity>>` indexed by `entity_id`.
Field paths into the entity schema are decoded using a Huffman tree
(see `second_pass/path_ops.rs`). The decoder dispatches to `cls_by_id[cls_id]`
which contains the `Serializer` tree with per-field decoders and `FieldInfo`.

`DemFullPacket` causes a full entity state reset using the cached `baselines`
before replaying the deltas — this is how the multi-threaded second pass can
start from any `DemFullPacket` offset independently.

---

## NAPI layer (`src/node/src/lib.rs`)

- `resolve_byte_type(path_or_buf)` → `(BytesVariant, Option<String>)`.
  Returns the file path when the input was a path (used as cache key).
- `make_parser(settings, mode, file_path)` — helper that calls
  `Parser::new(...).with_file_path(file_path)`.
- `parse_demo(bytes, &mut parser)` — dispatches over `BytesVariant`
  (Mmap or Vec) and calls `parser.parse_demo(...)`.
- All NAPI functions follow the pattern:
  ```rust
  let (bytes, file_path) = resolve_byte_type(path_or_buf)?;
  // … build settings …
  let mut parser = make_parser(settings, mode, file_path);
  let output = parse_demo(bytes, &mut parser)?;
  ```

### Scoped event parsing (`parseEventsScoped`)

- The Node binding now passes scoped event configuration into the **core parser** via
  `Parser::with_scoped_event_specs(...)` in `src/parser/src/parse_demo.rs`.
- `SecondPassParser` stores these specs in `scoped_event_specs: Arc<Vec<ScopedEventSpec>>`
  (`src/parser/src/second_pass/parser_settings.rs`).
- The core event implementation lives in `src/parser/src/second_pass/game_events.rs`.

#### ScopedEventSpec model
- `ScopedEventSpec` currently carries:
  - `event`
  - `player_props`
  - `other_props`
  - `where_clause`
  - `include_all_players`
  - `tick_filter`

#### `where_clause` vs `tick_filter`
- `where_clause` is used to decide whether a returned event matches a scoped spec.
- `tick_filter` is **not** a JS callback; it is a declarative core-side filter evaluated in Rust.
- `tick_filter` is evaluated against **raw event fields** before `find_extra(...)` runs.
- If `tick_filter` fails:
  - the event is still emitted
  - scoped extra props are skipped
  - `all_players` is skipped
- `tick_filter` currently supports:
  - `eq`
  - `neq`
  - `in`
- The right-hand side can be either:
  - a static `Variant`
  - another event field (`TickFilterValue::Field`)

#### `include_all_players`
- `include_all_players` is now built in the **same second pass** as event parsing.
- It no longer triggers a Node-side follow-up `parseTicks(orderBySteamid=true)` call.
- The snapshot is attached to `GameEvent.all_players` and serialized from the core.
- The snapshot is built from the current live `self.players` state at the event tick and only includes the requested scoped player props.

#### Filtering stages in the current implementation
- Raw event decode
- Scoped `tick_filter` gate for expensive enrichment
- `find_extra(...)` scoped extra prop materialization (only when gated in)
- `push_scoped_game_event(...)` optional `all_players` snapshot attachment
- Node-side final field filtering to hide props not requested by the matching scoped spec(s)

#### Important invariants
- `tick_filter` is **raw-event-field-only** in the current implementation.
  It cannot reference enriched fields like `user_X`, `game_time`, or `all_players`.
- `parseEventsScoped` is still expected to return the base event stream even when `tick_filter` fails.
  The filter only suppresses expensive enrichment, not event emission.
- If you add new custom events in `game_events.rs`, route them through `push_scoped_game_event(...)`
  instead of calling `self.game_events.push(...)` directly, otherwise scoped `all_players` snapshots are bypassed.
- If you create `GameEvent` literals in Rust tests, either use `GameEvent::new(...)` or set `all_players: None` explicitly.

#### Performance insight
- `parseEventsScoped` without `include_all_players` is the cheapest scoped workflow.
- `include_all_players` increases payload size and some per-event work, but remains much faster than re-running a second pass via `parseTicks`.

---

## Known quirks

- `SecondPassParser::new` has a no-op line:
  `first_pass_output.settings.wanted_player_props.clone().extend(...)` —
  this calls `.clone()` then `.extend()` on the temporary; the result is
  discarded. It appears to be a latent bug in the original code.

- `csgoproto` can be rebuilt from source — `protoc` is installed in this environment. Run `cargo build` inside `src/csgoproto/` to regenerate from the bundled `.proto` files.

- The `fallback_bytes` field in `ParserInputs` supplies a fallback game-event
  list for demos that are missing `SvcGameEventList` (some community servers).
  `parse_events` / `parse_event` expose this as `game_event_list_bytes`.

- `DemAnimationData` and `DemPacket` are skipped during the first pass
  (`is_packet_we_skip_on_first_pass`). The second pass processes `DemPacket`
  (where `SvcPacketEntities` and `SvcGameEvent` live) and skips
  `DemAnimationData`, `DemSendTables`, and `DemStringTables`.
