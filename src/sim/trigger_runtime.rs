//! Minimal runtime evaluation for map-authored triggers.
//!
//! This is intentionally narrow: it executes a small, high-value subset of
//! RA2/YR trigger behavior that already fits the current engine structure.
//! Supported today:
//! - Event 47: elapsed scenario time
//! - Event 27/28 and 36/37: global/local variable set/clear
//! - Action 22: force trigger
//! - Action 28/29: set/clear global variable
//! - Action 53/54: enable/disable trigger
//! - Action 48/112: center camera at waypoint
//!
//! The goal is to turn parsed trigger data into real runtime behavior without
//! committing to a full mission-script system yet.

use std::collections::{HashSet, VecDeque};

use crate::map::actions::{ActionEntry, ActionMap};
use crate::map::events::{EventCondition, EventMap};
use crate::map::trigger_graph::{LinkedTrigger, TriggerGraph};
use crate::map::triggers::TriggerMap;
use crate::map::variable_names::LocalVariableMap;
use crate::sim::world::Simulation;

const ACTION_FORCE_TRIGGER: i32 = 22;
const ACTION_SET_GLOBAL: i32 = 28;
const ACTION_CLEAR_GLOBAL: i32 = 29;
const ACTION_CENTER_CAMERA: i32 = 48;
const ACTION_ENABLE_TRIGGER: i32 = 53;
const ACTION_DISABLE_TRIGGER: i32 = 54;
const ACTION_SET_LOCAL: i32 = 56;
const ACTION_CLEAR_LOCAL: i32 = 57;
const ACTION_ANNOUNCE_WIN: i32 = 67;
const ACTION_ANNOUNCE_LOSE: i32 = 68;
const ACTION_END_SCENARIO: i32 = 69;
const ACTION_JUMP_CAMERA: i32 = 112;

const EVENT_GLOBAL_IS_SET: i32 = 27;
const EVENT_GLOBAL_IS_CLEAR: i32 = 28;
const EVENT_LOCAL_IS_SET: i32 = 36;
const EVENT_LOCAL_IS_CLEAR: i32 = 37;
const EVENT_ELAPSED_SCENARIO_TIME: i32 = 47;
const EVENT_TECHTYPE_EXISTS: i32 = 60;
const EVENT_TECHTYPE_DOES_NOT_EXIST: i32 = 61;

#[derive(Debug, Clone, PartialEq)]
pub enum TriggerEffect {
    CenterCameraAtWaypoint { waypoint: u32, immediate: bool },
    MissionAnnouncement { text: String },
    MissionResult { title: String, detail: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MissionAnnouncementKind {
    Victory,
    Defeat,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TriggerRuntime {
    pub elapsed_scenario_ticks: u64,
    pub globals_set: HashSet<u32>,
    pub locals_set: HashSet<u32>,
    pub disabled_triggers: HashSet<String>,
    pub fired_one_shot_triggers: HashSet<String>,
    last_announcement: Option<MissionAnnouncementKind>,
}

impl TriggerRuntime {
    pub fn from_map(triggers: &TriggerMap, local_variables: &LocalVariableMap) -> Self {
        let mut runtime = TriggerRuntime::default();
        for trigger in triggers.values() {
            if !trigger.enabled || !trigger.difficulty.medium {
                runtime.disabled_triggers.insert(trigger.id.clone());
            }
        }
        for local in local_variables.values() {
            if local.initially_set {
                runtime.locals_set.insert(local.index);
            }
        }
        runtime
    }

    pub fn advance(
        &mut self,
        tick_count: u32,
        graph: &TriggerGraph,
        triggers: &TriggerMap,
        events: &EventMap,
        actions: &ActionMap,
        simulation: Option<&Simulation>,
    ) -> Vec<TriggerEffect> {
        self.elapsed_scenario_ticks = self
            .elapsed_scenario_ticks
            .saturating_add(u64::from(tick_count));
        let linked_by_id: std::collections::HashMap<&str, &LinkedTrigger> = graph
            .triggers
            .iter()
            .map(|linked| (linked.trigger_id.as_str(), linked))
            .collect();

        let mut queue: VecDeque<String> = graph
            .triggers
            .iter()
            .filter(|linked| self.is_trigger_ready(linked, triggers, events, simulation))
            .map(|linked| linked.trigger_id.clone())
            .collect();
        let mut queued: HashSet<String> = queue.iter().cloned().collect();
        let mut effects: Vec<TriggerEffect> = Vec::new();

        while let Some(trigger_id) = queue.pop_front() {
            queued.remove(&trigger_id);
            let Some(trigger) = triggers.get(&trigger_id) else {
                continue;
            };
            let Some(linked) = linked_by_id.get(trigger_id.as_str()).copied() else {
                continue;
            };
            if !self.is_trigger_ready(linked, triggers, events, simulation) {
                continue;
            }

            if let Some(action) = actions.get(&trigger_id) {
                for entry in &action.entries {
                    self.apply_action(entry, &mut effects, &mut queue, &mut queued, triggers);
                }
            }

            if let Some(linked_trigger_id) = &trigger.linked_trigger_id {
                if triggers.contains_key(linked_trigger_id) {
                    enqueue_trigger(&mut queue, &mut queued, linked_trigger_id.clone());
                }
            }

            if !trigger.repeating {
                self.fired_one_shot_triggers.insert(trigger_id);
            }
        }

        effects
    }

    fn is_trigger_ready(
        &self,
        linked: &LinkedTrigger,
        triggers: &TriggerMap,
        events: &EventMap,
        simulation: Option<&Simulation>,
    ) -> bool {
        if self.disabled_triggers.contains(&linked.trigger_id) {
            return false;
        }

        let Some(trigger) = triggers.get(&linked.trigger_id) else {
            return false;
        };
        if !trigger.repeating && self.fired_one_shot_triggers.contains(&linked.trigger_id) {
            return false;
        }

        let Some(event_id) = &linked.event_id else {
            return false;
        };
        let Some(event) = events.get(event_id) else {
            return false;
        };
        !event.conditions.is_empty()
            && event
                .conditions
                .iter()
                .all(|condition| self.evaluate_event(condition, simulation))
    }

    fn evaluate_event(&self, condition: &EventCondition, simulation: Option<&Simulation>) -> bool {
        match condition.kind {
            EVENT_ELAPSED_SCENARIO_TIME => parse_u32_param(&condition.params, 0)
                .is_some_and(|seconds| self.elapsed_scenario_ticks >= u64::from(seconds) * 30),
            EVENT_GLOBAL_IS_SET => parse_u32_param(&condition.params, 0)
                .is_some_and(|index| self.globals_set.contains(&index)),
            EVENT_GLOBAL_IS_CLEAR => parse_u32_param(&condition.params, 0)
                .is_some_and(|index| !self.globals_set.contains(&index)),
            EVENT_LOCAL_IS_SET => parse_u32_param(&condition.params, 0)
                .is_some_and(|index| self.locals_set.contains(&index)),
            EVENT_LOCAL_IS_CLEAR => parse_u32_param(&condition.params, 0)
                .is_some_and(|index| !self.locals_set.contains(&index)),
            EVENT_TECHTYPE_EXISTS => {
                let Some(sim) = simulation else { return false };
                let min_count = parse_u32_param(&condition.params, 0).unwrap_or(1);
                let Some(type_id) = condition.params.get(1).map(|value| value.trim()) else {
                    return false;
                };
                if type_id.is_empty() {
                    return false;
                }
                count_techtype(sim, type_id) >= min_count as usize
            }
            EVENT_TECHTYPE_DOES_NOT_EXIST => {
                let Some(sim) = simulation else { return false };
                let Some(type_id) = condition.params.get(1).map(|value| value.trim()) else {
                    return false;
                };
                if type_id.is_empty() {
                    return false;
                }
                count_techtype(sim, type_id) == 0
            }
            _ => false,
        }
    }

    fn apply_action(
        &mut self,
        action: &ActionEntry,
        effects: &mut Vec<TriggerEffect>,
        queue: &mut VecDeque<String>,
        queued: &mut HashSet<String>,
        triggers: &TriggerMap,
    ) {
        match action.kind {
            ACTION_FORCE_TRIGGER => {
                if let Some(target) = parse_trigger_id_param(&action.params, 0) {
                    enqueue_trigger(queue, queued, target);
                }
            }
            ACTION_SET_GLOBAL => {
                if let Some(index) = parse_u32_param(&action.params, 0) {
                    self.globals_set.insert(index);
                }
            }
            ACTION_CLEAR_GLOBAL => {
                if let Some(index) = parse_u32_param(&action.params, 0) {
                    self.globals_set.remove(&index);
                }
            }
            ACTION_ENABLE_TRIGGER => {
                if let Some(target) = parse_trigger_id_param(&action.params, 0) {
                    self.disabled_triggers.remove(&target);
                    if triggers.contains_key(&target) {
                        enqueue_trigger(queue, queued, target);
                    }
                }
            }
            ACTION_DISABLE_TRIGGER => {
                if let Some(target) = parse_trigger_id_param(&action.params, 0) {
                    self.disabled_triggers.insert(target);
                }
            }
            ACTION_SET_LOCAL => {
                if let Some(index) = parse_u32_param(&action.params, 0) {
                    self.locals_set.insert(index);
                }
            }
            ACTION_CLEAR_LOCAL => {
                if let Some(index) = parse_u32_param(&action.params, 0) {
                    self.locals_set.remove(&index);
                }
            }
            ACTION_CENTER_CAMERA => {
                if let Some(waypoint) = parse_u32_param(&action.params, 6) {
                    effects.push(TriggerEffect::CenterCameraAtWaypoint {
                        waypoint,
                        immediate: false,
                    });
                }
            }
            ACTION_JUMP_CAMERA => {
                if let Some(waypoint) = parse_u32_param(&action.params, 6) {
                    effects.push(TriggerEffect::CenterCameraAtWaypoint {
                        waypoint,
                        immediate: true,
                    });
                }
            }
            ACTION_ANNOUNCE_WIN => {
                self.last_announcement = Some(MissionAnnouncementKind::Victory);
                effects.push(TriggerEffect::MissionAnnouncement {
                    text: "Mission Accomplished".to_string(),
                });
            }
            ACTION_ANNOUNCE_LOSE => {
                self.last_announcement = Some(MissionAnnouncementKind::Defeat);
                effects.push(TriggerEffect::MissionAnnouncement {
                    text: "Mission Failed".to_string(),
                });
            }
            ACTION_END_SCENARIO => {
                let (title, detail) = match self.last_announcement {
                    Some(MissionAnnouncementKind::Victory) => (
                        "Mission Accomplished".to_string(),
                        "The scenario ended after a victory announcement.".to_string(),
                    ),
                    Some(MissionAnnouncementKind::Defeat) => (
                        "Mission Failed".to_string(),
                        "The scenario ended after a defeat announcement.".to_string(),
                    ),
                    None => (
                        "Scenario Ended".to_string(),
                        "A map trigger ended the scenario.".to_string(),
                    ),
                };
                effects.push(TriggerEffect::MissionResult { title, detail });
            }
            _ => {}
        }
    }
}

fn enqueue_trigger(queue: &mut VecDeque<String>, queued: &mut HashSet<String>, trigger_id: String) {
    if queued.insert(trigger_id.clone()) {
        queue.push_back(trigger_id);
    }
}

fn parse_u32_param(fields: &[String], index: usize) -> Option<u32> {
    fields.get(index)?.trim().parse::<u32>().ok()
}

fn parse_trigger_id_param(fields: &[String], index: usize) -> Option<String> {
    let id = fields.get(index)?.trim();
    (!id.is_empty()).then(|| id.to_ascii_uppercase())
}

fn count_techtype(sim: &Simulation, type_id: &str) -> usize {
    sim.entities
        .values()
        .filter(|e| sim.interner.resolve(e.type_ref).eq_ignore_ascii_case(type_id))
        .count()
}

#[cfg(test)]
#[path = "trigger_runtime_tests.rs"]
mod tests;
