#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The Claude Code plugin files: they parse, they agree on the plugin name,
//! and every hook runs a command the CLI accepts.

use std::path::Path;

use clap::Parser;
use devslot::cli::args::{Cli, Command};
use serde_json::Value;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const HOOK_COMMAND_PREFIX: &str = "ds hook ";

fn read_json(relative: &str) -> Value {
    let text = std::fs::read_to_string(Path::new(ROOT).join(relative)).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn hook_commands() -> Vec<(String, String)> {
    let hooks = read_json("plugin/hooks/hooks.json");
    let mut commands = Vec::new();
    for (event, groups) in hooks["hooks"].as_object().unwrap() {
        for group in groups.as_array().unwrap() {
            for hook in group["hooks"].as_array().unwrap() {
                assert_eq!(hook["type"], "command", "{event}");
                commands.push((event.clone(), hook["command"].as_str().unwrap().to_owned()));
            }
        }
    }
    commands
}

#[test]
fn should_name_the_plugin_the_same_in_the_marketplace_and_the_manifest() {
    let marketplace = read_json(".claude-plugin/marketplace.json");
    let manifest = read_json("plugin/.claude-plugin/plugin.json");

    let listed = &marketplace["plugins"][0];
    assert_eq!(marketplace["name"], "devslot");
    assert_eq!(marketplace["owner"]["name"], "Sonny");
    assert_eq!(listed["name"], manifest["name"]);
    assert_eq!(listed["source"], "./plugin");
    assert_eq!(manifest["version"], "0.2.0");
    assert_ne!(manifest["description"].as_str().unwrap_or_default(), "");
}

#[test]
fn should_point_the_marketplace_source_at_a_folder_that_holds_the_manifest() {
    let marketplace = read_json(".claude-plugin/marketplace.json");

    let source = marketplace["plugins"][0]["source"].as_str().unwrap();

    assert!(
        Path::new(ROOT)
            .join(source)
            .join(".claude-plugin/plugin.json")
            .exists()
    );
}

#[test]
fn should_hook_the_four_events_with_their_timeouts() {
    let hooks = read_json("plugin/hooks/hooks.json");
    let timeout_of = |event: &str| hooks["hooks"][event][0]["hooks"][0]["timeout"].clone();

    assert_eq!(hooks["hooks"].as_object().unwrap().len(), 4);
    assert_eq!(timeout_of("WorktreeCreate"), 180);
    assert_eq!(timeout_of("WorktreeRemove"), 180);
    assert_eq!(timeout_of("PreToolUse"), 20);
    assert_eq!(timeout_of("SessionStart"), 10);
    assert_eq!(hooks["hooks"]["PreToolUse"][0]["matcher"], "Bash");
}

#[test]
fn should_run_the_pre_tool_use_hook_on_the_write_tools_with_a_shorter_timeout() {
    let hooks = read_json("plugin/hooks/hooks.json");

    let write_group = &hooks["hooks"]["PreToolUse"][1];

    assert_eq!(write_group["matcher"], "Edit|Write|MultiEdit|NotebookEdit");
    assert_eq!(write_group["hooks"][0]["command"], "ds hook pre-tool-use");
    assert_eq!(write_group["hooks"][0]["timeout"], 10);
}

#[test]
fn should_run_a_ds_hook_command_the_cli_accepts_for_every_hook() {
    let commands = hook_commands();

    assert_eq!(commands.len(), 5);
    for (event, command) in commands {
        assert!(command.starts_with(HOOK_COMMAND_PREFIX), "{event}");
        let words = command.split_whitespace();
        let cli = Cli::try_parse_from(words).unwrap();
        assert!(matches!(cli.command, Command::Hook(_)), "{event}");
    }
}

#[test]
fn should_match_each_event_to_its_hook_subcommand() {
    let expected = [
        ("WorktreeCreate", "ds hook worktree-create"),
        ("WorktreeRemove", "ds hook worktree-remove"),
        ("PreToolUse", "ds hook pre-tool-use"),
        ("PreToolUse", "ds hook pre-tool-use"),
        ("SessionStart", "ds hook session-start"),
    ];

    let mut commands = hook_commands();
    commands.sort();

    let mut wanted: Vec<(String, String)> = expected
        .iter()
        .map(|(event, command)| ((*event).to_owned(), (*command).to_owned()))
        .collect();
    wanted.sort();
    assert_eq!(commands, wanted);
}

#[test]
fn should_name_the_skill_devslot_in_its_front_matter() {
    let text =
        std::fs::read_to_string(Path::new(ROOT).join("plugin/skills/devslot/SKILL.md")).unwrap();

    let front_matter = text
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map(|(front_matter, _)| front_matter)
        .unwrap();
    assert!(front_matter.lines().any(|line| line == "name: devslot"));
    assert!(
        front_matter
            .lines()
            .any(|line| line.starts_with("description: "))
    );
}
