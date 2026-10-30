/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The scopes read lately (SPEC §memory.store): a project's notes and links, kept
//! between reads while nothing has written the store since. "Since" is SQLite's own
//! word for it - `PRAGMA data_version` on a connection the cache holds for nothing
//! else, which moves on every commit by any other connection: this process's pool,
//! the offline CLI, an agent's shell. Opening a note reads its scope twice (the
//! association and the neighbourhood), the review inbox and the map once more; a
//! project of two thousand notes takes a tenth of a second each time. The scopes and
//! that connection go after [IDLE] without a read, the remembered file hashes
//! ([crate::fresh::forget]) with them.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{ConnectOptions, Connection};
use tokio::time::Instant;

use crate::error::MemResult;
use crate::graph::Net;
use crate::model::Memory;
use crate::service::load_scope;

/// Scopes kept at most; the one read longest ago goes first.
pub const SCOPES_MAX: usize = 3;
/// How long the cache outlives its last read (the store pool's own idle timeout).
pub const IDLE: Duration = Duration::from_secs(600);

/// Every note of a scope (history included) and its link index.
#[derive(Default)]
pub struct Loaded {
    pub all: Vec<Memory>,
    pub net: Net,
}

pub(crate) struct Scopes {
    warm: tokio::sync::Mutex<Warm>,
    /// [IDLE] in milliseconds; a test shortens it.
    idle_ms: AtomicU64,
}

impl Default for Scopes {
    fn default() -> Scopes {
        Scopes { warm: Default::default(), idle_ms: AtomicU64::new(IDLE.as_millis() as u64) }
    }
}

#[derive(Default)]
struct Warm {
    /// The connection `PRAGMA data_version` is asked on; it never reads anything else.
    watch: Option<SqliteConnection>,
    /// (project id, the data version it was read at, the scope), the latest read last.
    kept: Vec<(String, i64, Arc<Loaded>)>,
    used: Option<Instant>,
    reaping: bool,
}

impl Scopes {
    /// `project_id`'s scope: the one kept when nothing has committed since it was read,
    /// else read through `conn` (and kept). A store whose data version cannot be had is
    /// read every time, as before the cache.
    pub(crate) async fn get(self: &Arc<Self>, path: &Path, conn: &mut SqliteConnection, project_id: &str) -> MemResult<Arc<Loaded>> {
        let mut warm = self.warm.lock().await;
        warm.used = Some(Instant::now());
        // Asked before the read: a commit landing during it moves the version past the
        // one the scope is kept at, so the next read misses rather than trusting it.
        let version = warm.version(path).await;
        if let Some(version) = version {
            if let Some(at) = warm.kept.iter().position(|(id, v, _)| id == project_id && *v == version) {
                let entry = warm.kept.remove(at);
                let scope = entry.2.clone();
                warm.kept.push(entry);
                return Ok(scope);
            }
        }
        let (all, net) = load_scope(conn, project_id).await?;
        let scope = Arc::new(Loaded { all, net });
        warm.kept.retain(|(id, _, _)| id != project_id);
        if let Some(version) = version {
            warm.kept.push((project_id.to_string(), version, scope.clone()));
            if warm.kept.len() > SCOPES_MAX {
                warm.kept.remove(0);
            }
            if !warm.reaping {
                warm.reaping = true;
                reap(Arc::downgrade(self));
            }
        }
        Ok(scope)
    }

    /// Drop every scope and close the watch connection (the service closing).
    pub(crate) async fn clear(&self) {
        self.warm.lock().await.release().await;
    }
}

impl Warm {
    /// The store's data version, on the watch connection opened on first use; None when
    /// it cannot be had (the connection is dropped and opened again next time).
    async fn version(&mut self, path: &Path) -> Option<i64> {
        if self.watch.is_none() {
            // Never a journal mode or any other write: the pool's connection set those.
            let opts =
                SqliteConnectOptions::new().filename(path).create_if_missing(false).busy_timeout(Duration::from_secs(5));
            self.watch = opts.connect().await.ok();
        }
        let conn = self.watch.as_mut()?;
        match sqlx::query_scalar::<_, i64>("PRAGMA data_version").fetch_one(&mut *conn).await {
            Ok(version) => Some(version),
            Err(_) => {
                self.release().await;
                None
            }
        }
    }

    async fn release(&mut self) {
        self.kept.clear();
        if let Some(conn) = self.watch.take() {
            let _ = conn.close().await;
        }
    }
}

/// Release the cache once it has gone [IDLE] without a read. One task while anything is
/// kept; it holds the cache only weakly, so a dropped service takes it down.
fn reap(scopes: Weak<Scopes>) {
    tokio::spawn(async move {
        loop {
            let Some(strong) = scopes.upgrade() else { return };
            let limit = Duration::from_millis(strong.idle_ms.load(Ordering::Relaxed));
            let mut warm = strong.warm.lock().await;
            let idle = warm.used.map(|at| at.elapsed()).unwrap_or(limit);
            if idle >= limit {
                warm.release().await;
                warm.reaping = false;
                crate::fresh::forget();
                return;
            }
            drop(warm);
            drop(strong);
            tokio::time::sleep(limit - idle).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::Kind;
    use crate::service::{SaveRequest, Target};
    use crate::{Actor, MemoryService};

    fn note(title: &str) -> SaveRequest {
        SaveRequest {
            kind: Kind::Knowledge,
            title: title.into(),
            content: format!("{title}, written down."),
            anchors: Vec::new(),
            refs: Vec::new(),
            keywords: Vec::new(),
            links: Vec::new(),
            replaces: None,
            allow_similar: true,
            domain: None,
            evidence: Vec::new(),
        }
    }

    struct Scratch {
        dir: std::path::PathBuf,
        service: MemoryService,
    }

    impl Scratch {
        fn new() -> Scratch {
            let dir = std::env::temp_dir().join(format!("swiss-mem-scopes-{}", swiss_core::util::random_hex(6)));
            std::fs::create_dir_all(dir.join("app").join(".git")).unwrap();
            let dir = dir.canonicalize().unwrap();
            let service = MemoryService::at(&dir.join("store.sqlite3"));
            Scratch { dir, service }
        }

        fn repo(&self) -> Target {
            Target::dir(self.dir.join("app").display().to_string())
        }

        async fn project(&self) -> String {
            let store = self.service.store().await.unwrap();
            let mut conn = store.read().await.unwrap();
            let place = self.service.locate(&mut conn, &self.repo()).await.unwrap();
            place.project_id().unwrap()
        }

        async fn scope(&self, project: &str) -> Arc<Loaded> {
            let store = self.service.store().await.unwrap();
            let mut conn = store.read().await.unwrap();
            self.service.scope(&mut conn, project).await.unwrap()
        }

        async fn done(self) {
            self.service.close().await;
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    #[tokio::test]
    async fn a_scope_is_kept_until_a_commit_and_read_again_after() {
        let s = Scratch::new();
        s.service.save(&s.repo(), &Actor::panel(), note("Busy writes back off")).await.unwrap();
        let project = s.project().await;
        let first = s.scope(&project).await;
        assert_eq!(first.all.len(), 1);
        assert!(Arc::ptr_eq(&first, &s.scope(&project).await), "nothing committed: the kept scope");
        s.service.save(&s.repo(), &Actor::panel(), note("Timestamps are UTC")).await.unwrap();
        let second = s.scope(&project).await;
        assert!(!Arc::ptr_eq(&first, &second), "a commit reads the scope again");
        assert_eq!(second.all.len(), 2);
        s.done().await;
    }

    #[tokio::test]
    async fn a_commit_from_another_connection_is_seen() {
        let s = Scratch::new();
        s.service.save(&s.repo(), &Actor::panel(), note("Busy writes back off")).await.unwrap();
        let project = s.project().await;
        let first = s.scope(&project).await;
        // Another process's write, as the offline CLI makes one: its own service, its own pool.
        let other = MemoryService::at(&s.dir.join("store.sqlite3"));
        other.save(&s.repo(), &Actor::panel(), note("Timestamps are UTC")).await.unwrap();
        other.close().await;
        let second = s.scope(&project).await;
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(second.all.len(), 2);
        s.done().await;
    }

    #[tokio::test]
    async fn the_cache_goes_after_it_idles() {
        // A real clock, shortened: a paused one would time the pool's acquire out while
        // SQLite works on its own thread.
        let s = Scratch::new();
        s.service.scopes.idle_ms.store(1000, Ordering::Relaxed);
        s.service.save(&s.repo(), &Actor::panel(), note("Busy writes back off")).await.unwrap();
        let project = s.project().await;
        let first = s.scope(&project).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(Arc::ptr_eq(&first, &s.scope(&project).await), "a read within the idle window keeps it");
        tokio::time::sleep(Duration::from_secs(3)).await;
        let warm = s.service.scopes.warm.lock().await;
        assert!(warm.kept.is_empty() && warm.watch.is_none() && !warm.reaping, "idle: scopes and the watch connection gone");
        drop(warm);
        assert!(!Arc::ptr_eq(&first, &s.scope(&project).await), "the next read reads the scope again");
        s.done().await;
    }
}
