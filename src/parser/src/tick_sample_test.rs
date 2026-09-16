#![allow(non_snake_case)]

use crate::first_pass::parser_settings::rm_user_friendly_names;
use crate::first_pass::parser_settings::ParserInputs;
use crate::parse_demo::Parser;
use crate::parse_demo::ParsingMode;
use crate::second_pass::game_events::GameEvent;
use crate::second_pass::game_events::ScopedEventSpec;
use crate::second_pass::game_events::TrackColumns;
use crate::second_pass::variants::Variant;
use crate::second_pass::parser_settings::huffman_lookup_table;
use ahash::AHashMap;
use memmap2::MmapOptions;
use std::fs::File;

fn tick_sample_settings<'a>(huf: &'a Vec<(u8, u8)>) -> (ParserInputs<'a>, Vec<ScopedEventSpec>) {
    let player_props = vec![
        "X".to_string(),
        "Y".to_string(),
        "Z".to_string(),
        "yaw".to_string(),
        "is_alive".to_string(),
        "team_num".to_string(),
    ];
    let real_player_props = rm_user_friendly_names(&player_props).unwrap();
    let mut real_name_to_og_name = AHashMap::default();
    for (real_name, user_friendly_name) in real_player_props.iter().zip(player_props.iter()) {
        real_name_to_og_name.insert(real_name.clone(), user_friendly_name.clone());
    }
    let settings = ParserInputs {
        real_name_to_og_name,
        wanted_players: vec![],
        wanted_player_props: real_player_props.clone(),
        wanted_other_props: vec![],
        wanted_prop_states: AHashMap::default(),
        wanted_ticks: vec![],
        wanted_events: vec!["tick_sample".to_string()],
        parse_ents: true,
        parse_projectiles: false,
        parse_grenades: false,
        only_header: true,
        only_convars: false,
        huffman_lookup_table: &huf,
        order_by_steamid: false,
        list_props: false,
        fallback_bytes: None,
    };
    let specs = vec![ScopedEventSpec {
        event: "tick_sample".to_string(),
        player_props: real_player_props,
        other_props: vec![],
        where_clause: AHashMap::default(),
        include_all_players: false,
        tick_filter: vec![],
        sample_every_ticks: Some(8),
    }];
    (settings, specs)
}

fn parse_tick_sample(mode: ParsingMode) -> Vec<GameEvent> {
    let huf = huffman_lookup_table();
    let (settings, specs) = tick_sample_settings(&huf);
    let file = File::open("test_demo.dem").unwrap();
    let mmap = unsafe { MmapOptions::new().map(&file).unwrap() };
    let mut parser = Parser::new(settings, mode).with_scoped_event_specs(specs);
    parser
        .parse_demo(&mmap)
        .unwrap()
        .game_events
        .into_iter()
        .filter(|event| event.name == "tick_sample")
        .collect()
}

fn parse_tick_sample_sidecar(mode: ParsingMode) -> (TrackColumns, Vec<GameEvent>) {
    let huf = huffman_lookup_table();
    let (settings, specs) = tick_sample_settings(&huf);
    let file = File::open("test_demo.dem").unwrap();
    let mmap = unsafe { MmapOptions::new().map(&file).unwrap() };
    let mut parser = Parser::new(settings, mode)
        .with_scoped_event_specs(specs)
        .with_track_sidecar(true);
    let output = parser.parse_demo(&mmap).unwrap();
    (output.track_cols, output.game_events)
}

/// (tick, steamid) -> (X, Y, Z, yaw, is_alive, team_num) as the sidecar encodes them.
fn snapshot_rows(events: &[GameEvent]) -> std::collections::BTreeMap<(i32, u64), ([f32; 4], u8, u8)> {
    let f = |v: Option<&Option<Variant>>| match v {
        Some(Some(Variant::F32(x))) => *x,
        Some(Some(Variant::I32(x))) => *x as f32,
        Some(Some(Variant::U32(x))) => *x as f32,
        _ => f32::NAN,
    };
    let u = |v: Option<&Option<Variant>>| match v {
        Some(Some(Variant::Bool(b))) => *b as u8,
        Some(Some(Variant::U32(x))) => *x as u8,
        Some(Some(Variant::I32(x))) => *x as u8,
        _ => u8::MAX,
    };
    let mut rows = std::collections::BTreeMap::new();
    for event in events {
        let Some(all_players) = event.all_players.as_ref() else { continue };
        for (steamid, snap) in all_players {
            rows.insert(
                (event.tick, steamid.parse::<u64>().unwrap()),
                (
                    [f(snap.get("X")), f(snap.get("Y")), f(snap.get("Z")), f(snap.get("yaw"))],
                    u(snap.get("is_alive")),
                    u(snap.get("team_num")),
                ),
            );
        }
    }
    rows
}

fn sidecar_rows(cols: &TrackColumns) -> Vec<((i32, u64), ([f32; 4], u8, u8))> {
    (0..cols.len())
        .map(|i| {
            (
                (cols.tick[i], cols.steamid[i]),
                ([cols.x[i], cols.y[i], cols.z[i], cols.yaw[i]], cols.is_alive[i], cols.team_num[i]),
            )
        })
        .collect()
}

fn same_f32(a: f32, b: f32) -> bool {
    (a.is_nan() && b.is_nan()) || a == b
}

#[test]
fn tick_sample_sidecar_matches_event_snapshots_and_emits_no_events() {
    let events = parse_tick_sample(ParsingMode::ForceSingleThreaded);
    let (cols, game_events) = parse_tick_sample_sidecar(ParsingMode::ForceSingleThreaded);

    assert!(!game_events.iter().any(|e| e.name == "tick_sample"), "sidecar mode must not emit tick_sample rows");
    assert!(cols.len() > 0);
    for col_len in [cols.steamid.len(), cols.x.len(), cols.y.len(), cols.z.len(), cols.yaw.len(), cols.is_alive.len(), cols.team_num.len()] {
        assert_eq!(col_len, cols.len());
    }

    let expected = snapshot_rows(&events);
    let actual = sidecar_rows(&cols);
    assert_eq!(actual.len(), expected.len(), "one sidecar row per (tick, player) snapshot");
    let mut last_tick = i32::MIN;
    for (key, (xyzw, is_alive, team_num)) in &actual {
        assert!(key.0 >= last_tick, "rows must be grouped by ascending tick");
        last_tick = key.0;
        let (exp_xyzw, exp_alive, exp_team) = expected.get(key).unwrap_or_else(|| panic!("no snapshot for {key:?}"));
        for axis in 0..4 {
            assert!(same_f32(xyzw[axis], exp_xyzw[axis]), "{key:?} axis {axis}: {} vs {}", xyzw[axis], exp_xyzw[axis]);
        }
        assert_eq!(is_alive, exp_alive, "{key:?} is_alive");
        assert_eq!(team_num, exp_team, "{key:?} team_num");
    }
}

#[test]
fn tick_sample_sidecar_st_mt_outputs_are_identical() {
    let (single, _) = parse_tick_sample_sidecar(ParsingMode::ForceSingleThreaded);
    let (multi, _) = parse_tick_sample_sidecar(ParsingMode::ForceMultiThreaded);
    assert!(single.len() > 0);
    assert_eq!(single.len(), multi.len());
    let single_rows = sidecar_rows(&single);
    let multi_rows = sidecar_rows(&multi);
    for (a, b) in single_rows.iter().zip(&multi_rows) {
        assert_eq!(a.0, b.0);
        for axis in 0..4 {
            assert!(same_f32(a.1 .0[axis], b.1 .0[axis]));
        }
        assert_eq!(a.1 .1, b.1 .1);
        assert_eq!(a.1 .2, b.1 .2);
    }
}

#[test]
fn track_columns_extend_dedup_drops_boundary_duplicates() {
    let mk = |ticks: &[i32]| {
        let mut c = TrackColumns::default();
        for (i, t) in ticks.iter().enumerate() {
            c.tick.push(*t);
            c.steamid.push(i as u64);
            c.x.push(i as f32);
            c.y.push(0.0);
            c.z.push(0.0);
            c.yaw.push(0.0);
            c.is_alive.push(1);
            c.team_num.push(2);
        }
        c
    };
    let mut first = mk(&[8, 8, 16, 16]);
    let second = mk(&[16, 16, 24, 24]);
    first.extend_dedup(second);
    assert_eq!(first.tick, vec![8, 8, 16, 16, 24, 24]);
    assert_eq!(first.steamid, vec![0, 1, 2, 3, 2, 3]);
    assert_eq!(first.len(), 6);

    let mut empty = TrackColumns::default();
    empty.extend_dedup(mk(&[0, 0]));
    assert_eq!(empty.tick, vec![0, 0]);
}

fn sample_signature(event: &GameEvent) -> (i32, Vec<(String, Vec<(String, String)>)>) {
    let mut players: Vec<(String, Vec<(String, String)>)> = event
        .all_players
        .as_ref()
        .map(|all_players| {
            all_players
                .iter()
                .map(|(steamid, snapshot)| {
                    let mut props: Vec<(String, String)> = snapshot
                        .iter()
                        .map(|(prop_name, value)| (prop_name.clone(), format!("{:?}", value)))
                        .collect();
                    props.sort_by(|left, right| left.0.cmp(&right.0));
                    (steamid.clone(), props)
                })
                .collect()
        })
        .unwrap_or_default();
    players.sort_by(|left, right| left.0.cmp(&right.0));
    (event.tick, players)
}

#[test]
fn tick_sample_st_mt_outputs_are_identical() {
    let single_threaded = parse_tick_sample(ParsingMode::ForceSingleThreaded);
    let multi_threaded = parse_tick_sample(ParsingMode::ForceMultiThreaded);

    assert!(!single_threaded.is_empty());
    assert_eq!(single_threaded.len(), multi_threaded.len());

    let single_signatures: Vec<_> = single_threaded.iter().map(sample_signature).collect();
    let multi_signatures: Vec<_> = multi_threaded.iter().map(sample_signature).collect();
    assert_eq!(single_signatures, multi_signatures);

    let mut seen_ticks = std::collections::BTreeSet::new();
    for event in &single_threaded {
        assert_eq!(event.tick.rem_euclid(8), 0);
        assert!(event.tick >= 0);
        assert!(seen_ticks.insert(event.tick));
        let all_players = event.all_players.as_ref().expect("tick_sample rows include all_players");
        assert!(!all_players.is_empty());
        for player_snapshot in all_players.values() {
            for prop_name in ["X", "Y", "Z", "yaw", "is_alive", "team_num"] {
                assert!(
                    player_snapshot.contains_key(prop_name),
                    "missing {prop_name} on tick {}",
                    event.tick
                );
            }
        }
    }
}
