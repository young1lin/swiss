/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The manifest's `machine` section (SPEC §host.setup): the user's PATH and the autostart seat,
//! through the routes the panel uses, and the skills. Skills land in this process, as
//! `swiss skill install` does, because the daemon's `CLAUDE_CONFIG_DIR` need not be the Agent's.

use std::collections::BTreeMap;

use reqwest::Method;
use serde_json::{json, Value};

use super::apply::fail;
use super::refuse::{join, nearest, Problem};
use super::report::{Item, Status};
use crate::remote_cli::Gateway;
use crate::skill_install::{shipped_names, CopyState, Places};
use swiss_core::secure::literal;

const KEYS: [&str; 3] = ["path", "autostart", "skills"];

/// A seat's key in the manifest, and the route that holds it.
const SEATS: [(&str, &str); 2] = [("path", "/api/user-path"), ("autostart", "/api/autostart")];

const SKILLS_RULE: &str = "skills lists the skills the user chose, by name, and never removes one";

pub fn schema() -> Value {
    json!({ "path": "true or false; left out, the user's PATH stays as it is",
        "autostart": "true or false; left out, autostart stays as it is",
        "skills": SKILLS_RULE, "shippedSkills": shipped_names() })
}

#[derive(Debug, Default)]
pub struct Machine {
    pub path: Option<bool>,
    pub autostart: Option<bool>,
    pub skills: Vec<String>,
}

impl Machine {
    fn seat(&self, key: &str) -> Option<bool> {
        match key {
            "path" => self.path,
            "autostart" => self.autostart,
            _ => None,
        }
    }
}

pub fn read_section(section: Option<&Value>, problems: &mut Vec<Problem>) -> Machine {
    let mut machine = Machine::default();
    let Some(section) = section.filter(|section| !section.is_null()) else {
        return machine;
    };
    let Some(fields) = section.as_object() else {
        problems.push(Problem::new("machine", "machine holds path, autostart and skills"));
        return machine;
    };
    for key in fields.keys().filter(|key| !KEYS.contains(&key.as_str())) {
        problems.push(Problem::unknown_field(join("machine", key), key, KEYS));
    }
    let mut seat = |key: &str| match fields.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::Bool(wanted)) => Some(*wanted),
        Some(_) => {
            problems.push(Problem::new(join("machine", key), format!("{key} is true or false; left out, it stays as it is")));
            None
        }
    };
    machine.path = seat("path");
    machine.autostart = seat("autostart");
    if let Some(skills) = fields.get("skills").filter(|skills| !skills.is_null()) {
        machine.skills = read_skills(skills, problems);
    }
    machine
}

fn read_skills(value: &Value, problems: &mut Vec<Problem>) -> Vec<String> {
    let shipped = shipped_names();
    let known: Vec<&str> = shipped.iter().map(String::as_str).collect();
    let Some(names) = value.as_array() else {
        problems.push(Problem::new("machine.skills", format!("{SKILLS_RULE}; this build ships: {}", known.join(", "))));
        return Vec::new();
    };
    let mut chosen: Vec<String> = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let path = format!("machine.skills[{index}]");
        match name.as_str() {
            Some(name) if known.contains(&name) => {
                if !chosen.iter().any(|already| already == name) {
                    chosen.push(name.to_string());
                }
            }
            Some(name) => {
                let message = match nearest(name, &known) {
                    Some(near) => format!("no such skill; did you mean {near}?"),
                    None => format!("no such skill; this build ships: {}", known.join(", ")),
                };
                problems.push(Problem::new(path, message));
            }
            None => problems.push(Problem::new(path, "a skill is named by a string")),
        }
    }
    chosen
}

/// What this machine holds of what the section names.
#[derive(Debug, Default)]
pub struct MachineState {
    /// Each named seat: whether it is on, or why that cannot be read.
    seats: BTreeMap<&'static str, Result<bool, String>>,
    /// Each named skill: its copy in each directory, or why that cannot be read.
    skills: BTreeMap<String, Result<Vec<CopyState>, String>>,
}

pub async fn read_state(gw: &Gateway, machine: &Machine) -> MachineState {
    let mut state = MachineState::default();
    for (key, route) in SEATS {
        if machine.seat(key).is_some() {
            let seat = match gw.get(route).await {
                Ok(answer) => answer["enabled"].as_bool().ok_or_else(|| format!("{route} did not say whether it is on")),
                Err(error) => Err(literal::mask(&error)),
            };
            state.seats.insert(key, seat);
        }
    }
    if !machine.skills.is_empty() {
        let places = Places::of_this_user();
        for name in &machine.skills {
            let copies = match &places {
                Ok(places) => copies_of(places, name),
                Err(error) => Err(error.clone()),
            };
            state.skills.insert(name.clone(), copies);
        }
    }
    state
}

fn copies_of(places: &Places, name: &str) -> Result<Vec<CopyState>, String> {
    let mut copies = Vec::new();
    for skill in places.status().iter().filter(|skill| skill.name == name) {
        for place in &skill.locations {
            if place.state == CopyState::Unreadable {
                return Err(format!("cannot read {}: {}", place.path, place.error.as_deref().unwrap_or("no detail")));
            }
            copies.push(place.state);
        }
    }
    Ok(copies)
}

pub fn decide(machine: &Machine, state: &MachineState) -> Vec<Item> {
    let mut items = Vec::new();
    for (key, read) in &state.seats {
        let Some(wanted) = machine.seat(key) else {
            continue;
        };
        items.push(match read {
            Ok(on) if *on == wanted => Item::new("machine", key, Status::Unchanged),
            Ok(_) => Item::new("machine", key, Status::Updated),
            Err(error) => Item::new("machine", key, Status::Updated).failed("machine.unavailable", error),
        });
    }
    for (name, read) in &state.skills {
        items.push(match read {
            Ok(copies) if copies.contains(&CopyState::Absent) => Item::new("skill", name, Status::Created),
            Ok(copies) if copies.contains(&CopyState::Outdated) => Item::new("skill", name, Status::Updated),
            Ok(_) => Item::new("skill", name, Status::Unchanged),
            Err(error) => Item::new("skill", name, Status::Updated).failed("skill.read", error),
        });
    }
    items
}

pub async fn write(gw: &Gateway, machine: &Machine, items: &mut [Item]) {
    for item in items.iter_mut() {
        match (item.kind, item.status) {
            ("machine", Status::Updated) => write_seat(gw, machine, item).await,
            ("skill", Status::Created | Status::Updated) => {
                if let Err(error) = install(&item.name) {
                    fail(item, "skill.write", error);
                }
            }
            _ => {}
        }
    }
}

/// What the route answers is the seat read back, so a write it could not make shows here.
async fn write_seat(gw: &Gateway, machine: &Machine, item: &mut Item) {
    let found = SEATS.iter().find(|(key, _)| *key == item.name).map(|(_, route)| *route);
    let (Some(route), Some(wanted)) = (found, machine.seat(&item.name)) else {
        return;
    };
    match gw.send(Method::PUT, route, Some(json!({ "enabled": wanted }))).await {
        Ok(answer) if answer["enabled"] == json!(wanted) => {}
        Ok(answer) => {
            let detail = answer["detail"].as_str().unwrap_or("no detail");
            fail(item, "machine.write", format!("it did not change: {}", literal::mask(detail)));
        }
        Err(error) => fail(item, "machine.write", literal::mask(&error)),
    }
}

/// Every copy not already this binary's. The rest of each directory is the user's.
fn install(name: &str) -> Result<(), String> {
    let places = Places::of_this_user()?;
    for skill in places.status().iter().filter(|skill| skill.name == name) {
        for place in skill.locations.iter().filter(|place| place.state != CopyState::Current) {
            places.install_one(name, &place.root)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn told(section: Value) -> Vec<(String, String)> {
        let mut problems = Vec::new();
        read_section(Some(&section), &mut problems);
        problems.into_iter().map(|p| (p.path, p.message)).collect()
    }

    // SPEC §host.setup: `skills` names what the user chose, so nothing can stand for "all of them".
    #[test]
    fn a_manifest_skills_true_is_refused() {
        let problems = told(json!({ "skills": true }));
        assert_eq!(problems[0].0, "machine.skills");
        assert!(problems[0].1.contains("swiss-memory"), "{problems:?}");
        assert_eq!(told(json!({ "skills": ["swiss", "swiss-memry", 7] }))[0].1, "no such skill; did you mean swiss-memory?");
        assert_eq!(told(json!({ "skills": ["swiss", 7] }))[0].0, "machine.skills[1]");
        assert!(told(json!({ "skills": [] })).is_empty());
    }

    #[test]
    fn a_seat_is_true_or_false_and_a_typo_is_refused() {
        let problems = told(json!({ "path": "yes", "autostrat": true }));
        let paths: Vec<&str> = problems.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(paths, ["machine.autostrat", "machine.path"]);
        assert_eq!(problems[0].1, "unknown field; did you mean autostart?");
        let mut problems = Vec::new();
        let read = read_section(Some(&json!({ "path": true, "skills": ["swiss", "swiss"] })), &mut problems);
        assert_eq!((read.path, read.autostart), (Some(true), None));
        assert_eq!(read.skills, ["swiss"], "a name written twice is one choice");
    }

    // SPEC §host.setup: absent, current or outdated decide the status, and a copy that cannot be read
    // fails rather than being overwritten.
    #[test]
    fn a_skill_is_decided_by_its_copies() {
        let machine = Machine { path: Some(true), autostart: Some(false), skills: vec![] };
        let mut state = MachineState::default();
        state.seats.insert("path", Ok(true));
        state.seats.insert("autostart", Err("503 not here".into()));
        state.skills.insert("a".into(), Ok(vec![CopyState::Current, CopyState::Absent]));
        state.skills.insert("b".into(), Ok(vec![CopyState::Outdated, CopyState::Current]));
        state.skills.insert("c".into(), Ok(vec![CopyState::Current, CopyState::Current]));
        state.skills.insert("d".into(), Err("cannot read".into()));
        let items = decide(&machine, &state);
        let read: Vec<(&str, &str, Status)> = items.iter().map(|i| (i.kind, i.name.as_str(), i.status)).collect();
        assert_eq!(
            read,
            [
                ("machine", "autostart", Status::Failed),
                ("machine", "path", Status::Unchanged),
                ("skill", "a", Status::Created),
                ("skill", "b", Status::Updated),
                ("skill", "c", Status::Unchanged),
                ("skill", "d", Status::Failed),
            ]
        );
        assert_eq!(items[0].extra["code"], json!("machine.unavailable"));
        assert_eq!(items[5].extra["code"], json!("skill.read"));
    }
}
