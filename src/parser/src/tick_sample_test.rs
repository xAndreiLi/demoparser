#![allow(non_snake_case)]

use crate::first_pass::parser_settings::rm_user_friendly_names;
use crate::first_pass::parser_settings::ParserInputs;
use crate::parse_demo::Parser;
use crate::parse_demo::ParsingMode;
use crate::second_pass::game_events::GameEvent;
use crate::second_pass::game_events::ScopedEventSpec;
use crate::second_pass::parser_settings::huffman_lookup_table;
use ahash::AHashMap;
use memmap2::MmapOptions;
use std::fs::File;

fn parse_tick_sample(mode: ParsingMode) -> Vec<GameEvent> {
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
    let huf = huffman_lookup_table();
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
