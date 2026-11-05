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

//! The one document a manifest command prints, on success and on failure alike, and its exit code
//! (SPEC §host.setup). It never holds a credential, a token, a one-time link or a secret's value.

use serde_json::{json, Map, Value};

use super::manifest::Warning;
use super::refuse::Problem;

pub const EXIT_FAILED: i32 = 1;
pub const EXIT_REFUSED: i32 = 2;
pub const EXIT_NO_GATEWAY: i32 = 3;
pub const EXIT_NEEDS_HUMAN: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Plan,
    Apply,
}

impl Mode {
    pub fn word(self) -> &'static str {
        match self {
            Mode::Plan => "plan",
            Mode::Apply => "apply",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Created,
    Updated,
    Unchanged,
    Failed,
    Blocked,
    NeedsInput,
}

impl Status {
    /// The word `status` reads, and the key `summary` counts it under: a plan has written nothing,
    /// so it does not say `created`.
    fn words(self, mode: Mode) -> (&'static str, &'static str) {
        match (self, mode) {
            (Status::Created, Mode::Plan) => ("will-create", "willCreate"),
            (Status::Updated, Mode::Plan) => ("will-update", "willUpdate"),
            (Status::Created, Mode::Apply) => ("created", "created"),
            (Status::Updated, Mode::Apply) => ("updated", "updated"),
            (Status::Unchanged, _) => ("unchanged", "unchanged"),
            (Status::Failed, _) => ("failed", "failed"),
            (Status::Blocked, _) => ("blocked", "blocked"),
            (Status::NeedsInput, _) => ("needs-input", "needsInput"),
        }
    }
}

/// One thing the manifest names.
#[derive(Debug, Clone)]
pub struct Item {
    pub kind: &'static str,
    pub name: String,
    pub status: Status,
    pub extra: Map<String, Value>,
}

impl Item {
    pub fn new(kind: &'static str, name: &str, status: Status) -> Self {
        Item { kind, name: name.to_string(), status, extra: Map::new() }
    }

    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.note(key, value);
        self
    }

    pub fn note(&mut self, key: &str, value: impl Into<Value>) {
        self.extra.insert(key.to_string(), value.into());
    }

    fn check_failed(&self) -> bool {
        self.extra.get("check").is_some_and(|check| check["ok"] == json!(false))
    }

    fn to_json(&self, mode: Mode) -> Value {
        let mut out = Map::new();
        out.insert("kind".into(), json!(self.kind));
        out.insert("name".into(), json!(self.name));
        out.insert("status".into(), json!(self.status.words(mode).0));
        out.extend(self.extra.clone());
        Value::Object(out)
    }
}

/// What a command found, before it is turned into the document.
#[derive(Debug)]
pub struct Report {
    pub mode: Mode,
    pub gateway: Value,
    pub items: Vec<Item>,
    pub warnings: Vec<Warning>,
    pub next: Vec<Value>,
}

impl Report {
    pub fn next_step(&mut self, who: &str, what: String, why: &str) {
        let step = json!({ "who": who, "do": what, "why": why });
        if !self.next.contains(&step) {
            self.next.push(step);
        }
    }

    /// 1 when anything failed, a saved item's check included; else 4 when something waits for a
    /// human; else 0. `ok` is that 0.
    pub fn exit_code(&self) -> i32 {
        if self.items.iter().any(|i| i.status == Status::Failed || i.check_failed()) {
            EXIT_FAILED
        } else if self.items.iter().any(|i| matches!(i.status, Status::NeedsInput | Status::Blocked)) {
            EXIT_NEEDS_HUMAN
        } else {
            0
        }
    }

    pub fn finish(self) -> (Value, i32) {
        let code = self.exit_code();
        let mut summary = Map::new();
        for status in [
            Status::Created,
            Status::Updated,
            Status::Unchanged,
            Status::Failed,
            Status::Blocked,
            Status::NeedsInput,
        ] {
            let count = self.items.iter().filter(|i| i.status == status).count();
            summary.insert(status.words(self.mode).1.to_string(), json!(count));
        }
        let items: Vec<Value> = self.items.iter().map(|i| i.to_json(self.mode)).collect();
        let document = json!({
            "ok": code == 0,
            "mode": self.mode.word(),
            "gateway": self.gateway,
            "summary": summary,
            "items": items,
            "warnings": self.warnings,
            "next": self.next,
        });
        (document, code)
    }
}

/// A manifest refused before anything was written.
pub fn refused(mode: Mode, problems: &[Problem]) -> (Value, i32) {
    (json!({ "ok": false, "mode": mode.word(), "problems": problems }), EXIT_REFUSED)
}

/// No gateway to talk to.
pub fn no_gateway(mode: Mode, error: &str) -> (Value, i32) {
    (json!({ "ok": false, "mode": mode.word(), "error": error }), EXIT_NO_GATEWAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(items: Vec<Item>) -> Report {
        Report { mode: Mode::Apply, gateway: json!({}), items, warnings: Vec::new(), next: Vec::new() }
    }

    // SPEC §host.setup: exit 1 outranks exit 4, and 0 means every item converged.
    #[test]
    fn the_exit_code_says_who_acts_next() {
        let done = Item::new("mcp", "a", Status::Created);
        let waits = Item::new("secret", "b", Status::NeedsInput);
        let blocked = Item::new("mcp", "c", Status::Blocked);
        let failed = Item::new("mcp", "d", Status::Failed);
        let bad_check = Item::new("mcp", "e", Status::Created).with("check", json!({ "ok": false }));
        let good_check = Item::new("mcp", "f", Status::Created).with("check", json!({ "ok": true }));

        assert_eq!(report(vec![done.clone(), good_check]).exit_code(), 0);
        assert_eq!(report(vec![done.clone(), waits.clone()]).exit_code(), EXIT_NEEDS_HUMAN);
        assert_eq!(report(vec![blocked.clone()]).exit_code(), EXIT_NEEDS_HUMAN);
        assert_eq!(report(vec![waits.clone(), failed]).exit_code(), EXIT_FAILED);
        assert_eq!(report(vec![blocked, bad_check]).exit_code(), EXIT_FAILED);
        assert_eq!(report(Vec::new()).exit_code(), 0);
    }

    // SPEC §host.setup: "under `plan` the first two read `will-create` and `will-update`".
    #[test]
    fn a_plan_does_not_say_it_created_anything() {
        let items = vec![
            Item::new("secret", "a", Status::Created),
            Item::new("mcp", "b", Status::Updated),
            Item::new("mcp", "c", Status::Unchanged),
        ];
        let plan = Report { mode: Mode::Plan, ..report(items.clone()) };
        let (document, code) = plan.finish();
        assert_eq!(code, 0);
        assert_eq!(document["mode"], json!("plan"));
        assert_eq!(document["items"][0]["status"], json!("will-create"));
        assert_eq!(document["items"][1]["status"], json!("will-update"));
        assert_eq!(document["items"][2]["status"], json!("unchanged"));
        assert_eq!(document["summary"]["willCreate"], json!(1));
        assert!(document["summary"].get("created").is_none(), "{document}");

        let (document, _) = report(items).finish();
        assert_eq!(document["items"][0]["status"], json!("created"));
        assert_eq!(document["summary"]["created"], json!(1));
        assert_eq!(document["summary"]["needsInput"], json!(0));
    }

    #[test]
    fn a_step_is_not_repeated() {
        let mut report = report(Vec::new());
        report.next_step("human", "x".into(), "y");
        report.next_step("human", "x".into(), "y");
        assert_eq!(report.next.len(), 1);
    }
}
