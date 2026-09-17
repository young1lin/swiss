//! The per-project binding (docs/32 §24): a .swiss/remote.json file a repository
//! carries, discovered by walking up from the working directory. It is what turns
//! `swiss remote exec build -- ...` into `swiss remote exec --target dev --timeout
//! 2h -- ...` without the agent retyping anything: named actions, a default target,
//! sync excludes, a timeout policy.
//!
//! Plain JSON (not sealed - a repository carries it): a binding has no secrets,
//! only target ids and paths. Strict like every parser here; versioned so a
//! future schema can migrate instead of guess.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The binding file, relative to the project root it describes.
pub const BINDING_FILE: &str = ".swiss/remote.json";
/// The one schema version this build reads and writes.
pub const SCHEMA_VERSION: u64 = 1;

/// One named action: which target, which workspace-relative cwd, which deadline.
#[derive(Debug, Clone, PartialEq)]
pub struct BindingAction {
    pub name: String,
    pub target: String,
    pub workspace: Option<String>,
    pub timeout_ms: Option<u64>,
}

/// The whole binding.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectBinding {
    /// The directory the binding file was found in (the project root).
    pub root: PathBuf,
    pub project: Option<String>,
    pub default_target: Option<String>,
    pub actions: Vec<BindingAction>,
    pub sync_excludes: Vec<String>,
}

impl ProjectBinding {
    /// Parse one binding file's JSON (root recorded by the caller).
    pub fn parse(root: PathBuf, value: &Value) -> Result<Self, String> {
        let obj = value
            .as_object()
            .ok_or_else(|| "binding must be an object".to_string())?;
        let version = obj
            .get("schemaVersion")
            .and_then(Value::as_u64)
            .ok_or_else(|| "binding.schemaVersion must be a positive integer".to_string())?;
        if version != SCHEMA_VERSION {
            return Err(format!(
                "binding schemaVersion {version} is not {SCHEMA_VERSION}; update swiss",
            ));
        }
        for key in obj.keys() {
            if ![
                "schemaVersion",
                "project",
                "defaultTarget",
                "actions",
                "sync",
            ]
            .contains(&key.as_str())
            {
                return Err(format!("binding.{key} is not a known field"));
            }
        }
        let text = |key: &str| -> Result<Option<String>, String> {
            match obj.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.clone())),
                Some(_) => Err(format!("binding.{key} must be a non-empty string")),
            }
        };
        let mut actions = Vec::new();
        match obj.get("actions") {
            None | Some(Value::Null) => {}
            Some(Value::Object(map)) => {
                for (name, row) in map {
                    if name.trim().is_empty() {
                        return Err("binding action names must be non-empty".into());
                    }
                    let row = row
                        .as_object()
                        .ok_or_else(|| format!("binding.actions.{name} must be an object"))?;
                    for key in row.keys() {
                        if !["target", "workspace", "timeoutMs"].contains(&key.as_str()) {
                            return Err(format!(
                                "binding.actions.{name}.{key} is not a known field"
                            ));
                        }
                    }
                    let target = row
                        .get("target")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .ok_or_else(|| format!("binding.actions.{name}.target is required"))?;
                    let workspace = match row.get("workspace") {
                        None | Some(Value::Null) => None,
                        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
                        Some(_) => {
                            return Err(format!(
                                "binding.actions.{name}.workspace must be a string"
                            ));
                        }
                    };
                    let timeout_ms = match row.get("timeoutMs") {
                        None | Some(Value::Null) => None,
                        Some(Value::Number(n)) => {
                            Some(n.as_u64().filter(|ms| *ms > 0).ok_or_else(|| {
                                format!("binding.actions.{name}.timeoutMs must be positive")
                            })?)
                        }
                        Some(_) => {
                            return Err(format!(
                                "binding.actions.{name}.timeoutMs must be an integer"
                            ));
                        }
                    };
                    actions.push(BindingAction {
                        name: name.clone(),
                        target: target.to_string(),
                        workspace,
                        timeout_ms,
                    });
                }
            }
            Some(_) => return Err("binding.actions must be an object".into()),
        }
        let mut sync_excludes = Vec::new();
        match obj.get("sync") {
            None | Some(Value::Null) => {}
            Some(Value::Object(map)) => {
                for key in map.keys() {
                    if key != "exclude" {
                        return Err(format!("binding.sync.{key} is not a known field"));
                    }
                }
                match map.get("exclude") {
                    None | Some(Value::Null) => {}
                    Some(Value::Array(items)) => {
                        for item in items {
                            sync_excludes.push(item.as_str().map(str::to_string).ok_or_else(
                                || "binding.sync.exclude must be strings".to_string(),
                            )?);
                        }
                    }
                    Some(_) => return Err("binding.sync.exclude must be an array".into()),
                }
            }
            Some(_) => return Err("binding.sync must be an object".into()),
        }
        Ok(ProjectBinding {
            root,
            project: text("project")?,
            default_target: text("defaultTarget")?,
            actions,
            sync_excludes,
        })
    }

    /// Look for the binding starting at `start` and walking up (max 32 levels -
    /// past a drive root there is nowhere left to find one). The FIRST file wins,
    /// like .git: a nested project can carry its own binding.
    pub fn discover(start: &Path) -> Result<Option<Self>, String> {
        let mut dir = Some(start);
        let mut depth = 0;
        while let Some(at) = dir {
            depth += 1;
            if depth > 32 {
                return Ok(None);
            }
            let file = at.join(BINDING_FILE);
            if file.is_file() {
                let raw = std::fs::read_to_string(&file)
                    .map_err(|err| format!("read {}: {err}", file.display()))?;
                let value: Value = serde_json::from_str(&raw)
                    .map_err(|err| format!("parse {}: {err}", file.display()))?;
                let root = at.to_path_buf();
                return ProjectBinding::parse(root, &value).map(Some);
            }
            dir = at.parent();
        }
        Ok(None)
    }

    pub fn action(&self, name: &str) -> Option<&BindingAction> {
        self.actions.iter().find(|a| a.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("swiss-bind-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    #[test]
    fn a_binding_parses_with_defaults() {
        let value = json!({ "schemaVersion": 1, "project": "demo" });
        let binding = ProjectBinding::parse(PathBuf::from("/repo"), &value).expect("parses");
        assert_eq!(binding.project.as_deref(), Some("demo"));
        assert!(binding.actions.is_empty());
        assert!(binding.sync_excludes.is_empty());
    }

    #[test]
    fn a_full_binding_round_trips_the_fields_that_matter() {
        let value = json!({
            "schemaVersion": 1,
            "project": "demo",
            "defaultTarget": "dev",
            "actions": {
                "build": { "target": "dev", "workspace": "build/arm", "timeoutMs": 7200000 },
                "test": { "target": "dev" }
            },
            "sync": { "exclude": ["vendor/", "*.tmp"] }
        });
        let binding = ProjectBinding::parse(PathBuf::from("/repo"), &value).expect("parses");
        assert_eq!(binding.default_target.as_deref(), Some("dev"));
        let build = binding.action("build").expect("named");
        assert_eq!(build.target, "dev");
        assert_eq!(build.workspace.as_deref(), Some("build/arm"));
        assert_eq!(build.timeout_ms, Some(7_200_000));
        assert_eq!(binding.sync_excludes, vec!["vendor/", "*.tmp"]);
    }

    #[test]
    fn version_mismatches_and_unknown_fields_are_refused() {
        let bad_version = json!({ "schemaVersion": 2 });
        assert!(ProjectBinding::parse(PathBuf::from("/r"), &bad_version).is_err());
        let unknown = json!({ "schemaVersion": 1, "targets": [] });
        let err = ProjectBinding::parse(PathBuf::from("/r"), &unknown).unwrap_err();
        assert!(err.contains("binding.targets"), "{err}");
        let bad_action =
            json!({ "schemaVersion": 1, "actions": { "b": { "target": "x", "sudo": true } } });
        let err = ProjectBinding::parse(PathBuf::from("/r"), &bad_action).unwrap_err();
        assert!(err.contains("binding.actions.b.sudo"), "{err}");
    }

    #[test]
    fn discovery_walks_up_and_the_first_file_wins() {
        let dir = scratch();
        let outer = dir.join("repo");
        let inner = outer.join("apps/thing");
        std::fs::create_dir_all(&inner).expect("tree");
        std::fs::create_dir_all(outer.join(".swiss")).expect("swiss");
        std::fs::write(
            outer.join(".swiss/remote.json"),
            r#"{"schemaVersion":1,"project":"outer"}"#,
        )
        .expect("w");
        let found = ProjectBinding::discover(&inner)
            .expect("no io error")
            .expect("found");
        assert_eq!(found.project.as_deref(), Some("outer"));
        assert_eq!(found.root, outer);
        assert!(
            ProjectBinding::discover(&dir).unwrap().is_none(),
            "nothing above the repo"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
