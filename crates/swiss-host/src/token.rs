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

//! The live set of named bearer tokens — port of `token.ts`.
//!
//! Each client (Claude Code, Codex, …) gets its own token, so the interaction log can attribute
//! every request to a client and one client can be revoked without rotating the rest. `verify`
//! returns the matched record (not just true/false) so the caller knows WHO authenticated. Built
//! once at boot; every mutation persists through the ManagedStore.
//!
//! On first boot the pre-multi-token secret migrates to a "default" token, so clients already
//! configured with the old secret keep authenticating unchanged.

use std::sync::RwLock;

use swiss_core::util::{new_secret, random_hex, timing_safe_eq};

/// A token as the panel/API ever sees it — no secret.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicToken {
    pub id: String,
    pub label: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

/// A named bearer token: the secret a client presents, plus a human label to tell clients apart.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenRec {
    pub id: String,
    pub label: String,
    pub secret: String,
    pub created_at: String,
}

/// The slice of a ManagedStore a TokenManager needs — so tests can pass an in-memory stand-in.
pub trait TokenPersistence: Send + Sync {
    fn get_tokens(&self) -> Vec<TokenRec>;
    fn save_tokens(&self, tokens: &[TokenRec]);
}

/// id + label access so pick_copy_token works over PublicToken and TokenRec alike.
pub trait HasIdLabel {
    fn id(&self) -> &str;
    fn label(&self) -> &str;
}

impl HasIdLabel for PublicToken {
    fn id(&self) -> &str {
        &self.id
    }
    fn label(&self) -> &str {
        &self.label
    }
}

impl HasIdLabel for TokenRec {
    fn id(&self) -> &str {
        &self.id
    }
    fn label(&self) -> &str {
        &self.label
    }
}

/// Which named token copied connect commands should embed. A remembered id (from the panel's
/// "Use" button) wins while it still exists. Otherwise the seed token labeled `default` — so
/// copy works on a fresh panel with several tokens. If `default` is gone too, the first row.
pub fn pick_copy_token<'a, T: HasIdLabel>(
    list: &'a [T],
    remembered_id: Option<&str>,
) -> Option<&'a T> {
    if list.is_empty() {
        return None;
    }
    if let Some(id) = remembered_id {
        if let Some(found) = list.iter().find(|t| t.id() == id) {
            return Some(found);
        }
    }
    list.iter()
        .find(|t| t.label() == "default")
        .or_else(|| list.first())
}

fn safe_eq(a: &str, b: &str) -> bool {
    timing_safe_eq(a.as_bytes(), b.as_bytes())
}

pub struct TokenManager {
    items: RwLock<Vec<TokenRec>>,
    store: std::sync::Arc<dyn TokenPersistence>,
}

impl TokenManager {
    pub fn new(store: std::sync::Arc<dyn TokenPersistence>, seed: Option<&str>) -> Self {
        let stored = store.get_tokens();
        let items = if !stored.is_empty() {
            stored
        } else if let Some(seed) = seed {
            // Persist the migration so the seed is stable across restarts.
            let items = vec![TokenRec {
                id: "default".into(),
                label: "default".into(),
                secret: seed.to_string(),
                created_at: String::new(),
            }];
            store.save_tokens(&items);
            items
        } else {
            Vec::new()
        };
        Self {
            items: RwLock::new(items),
            store,
        }
    }

    /// Resolve a bearer secret to its token record, or None.
    pub fn verify(&self, secret: &str) -> Option<TokenRec> {
        self.items
            .read()
            .ok()?
            .iter()
            .find(|t| safe_eq(&t.secret, secret))
            .cloned()
    }

    /// All tokens, secrets stripped — for the panel and the list API.
    pub fn list(&self) -> Vec<PublicToken> {
        self.items
            .read()
            .map(|items| {
                items
                    .iter()
                    .map(|t| PublicToken {
                        id: t.id.clone(),
                        label: t.label.clone(),
                        created_at: t.created_at.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Option<TokenRec> {
        self.items.read().ok()?.iter().find(|t| t.id == id).cloned()
    }

    /// Create a token. The secret is returned here and stays readable via get() — see the note
    /// on GET /api/tokens/:id/secret for why it is not write-only.
    pub fn create(&self, label: &str) -> TokenRec {
        let rec = TokenRec {
            id: random_hex(6),
            label: {
                let l = label.trim();
                if l.is_empty() {
                    "token".into()
                } else {
                    l.to_string()
                }
            },
            secret: new_secret(),
            created_at: swiss_core::log::iso_now(),
        };
        if let Ok(mut items) = self.items.write() {
            items.push(rec.clone());
            self.store.save_tokens(&items);
        }
        rec
    }

    /// Rotate one token's secret (keeps id + label; the old secret stops working immediately).
    pub fn rotate(&self, id: &str) -> Option<TokenRec> {
        let mut items = self.items.write().ok()?;
        let rec = items.iter_mut().find(|t| t.id == id)?;
        rec.secret = new_secret();
        let out = rec.clone();
        self.store.save_tokens(&items);
        Some(out)
    }

    /// Revoke a token. Returns false when no token had that id.
    pub fn remove(&self, id: &str) -> bool {
        let Some(mut items) = self.items.write().ok() else {
            return false;
        };
        let before = items.len();
        items.retain(|t| t.id != id);
        let changed = items.len() < before;
        if changed {
            self.store.save_tokens(&items);
        }
        changed
    }
}

/// A TokenManager over a throwaway in-memory store, seeded with one token — for tests.
pub fn single_token_manager(secret: &str) -> TokenManager {
    struct Mem(std::sync::Mutex<Vec<TokenRec>>);
    impl TokenPersistence for Mem {
        fn get_tokens(&self) -> Vec<TokenRec> {
            self.0.lock().map(|v| v.clone()).unwrap_or_default()
        }
        fn save_tokens(&self, tokens: &[TokenRec]) {
            if let Ok(mut v) = self.0.lock() {
                *v = tokens.to_vec();
            }
        }
    }
    TokenManager::new(std::sync::Arc::new(Mem(Default::default())), Some(secret))
}

#[cfg(test)]
mod tests {
    // Ported from test/auth.test.ts's sibling token-pick assertions + token-pick.test.ts.
    use super::*;

    fn row(id: &str, label: &str) -> PublicToken {
        PublicToken {
            id: id.into(),
            label: label.into(),
            created_at: String::new(),
        }
    }

    #[test]
    fn pick_prefers_remembered_then_default_then_first() {
        let list = vec![row("a", "claude"), row("b", "default"), row("c", "codex")];
        assert_eq!(
            pick_copy_token(&list, Some("c")).map(|t| t.id.clone()),
            Some("c".into())
        );
        // A remembered id that no longer exists falls back to `default`.
        assert_eq!(
            pick_copy_token(&list, Some("gone")).map(|t| t.id.clone()),
            Some("b".into())
        );
        assert_eq!(
            pick_copy_token(&list, None).map(|t| t.id.clone()),
            Some("b".into())
        );
        let no_default = vec![row("z", "codex")];
        assert_eq!(
            pick_copy_token(&no_default, None).map(|t| t.id.clone()),
            Some("z".into())
        );
    }

    #[test]
    fn verify_seeds_and_rotates() {
        let mgr = single_token_manager("seed-secret");
        assert!(mgr.verify("wrong").is_none());
        let rec = mgr
            .verify("seed-secret")
            .expect("seed migrated to the default token");
        assert_eq!(rec.id, "default");
        let rotated = mgr.rotate("default").expect("rotates");
        assert!(
            mgr.verify("seed-secret").is_none(),
            "old secret stops working at once"
        );
        assert!(mgr.verify(&rotated.secret).is_some());
        assert!(mgr.remove("default"));
        assert!(!mgr.remove("default"));
        assert!(mgr.verify(&rotated.secret).is_none());
    }

    #[test]
    fn create_trims_and_defaults_the_label() {
        let mgr = single_token_manager("s");
        let a = mgr.create("  my client  ");
        assert_eq!(a.label, "my client");
        let b = mgr.create("");
        assert_eq!(b.label, "token");
        assert_eq!(mgr.list().len(), 3);
    }
}
