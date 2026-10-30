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

//! The pool (SPEC §memory.settings The pool): the workers one run asks, behind one [Ask].
//! Each question goes to the member with the most room - its free seats over its own
//! `concurrency`, the first listed on a tie - and waits for a seat there, so no member is
//! ever asked more at once than its agent allows, however the run and a lead's delegated
//! tasks overlap. A question a member fails is asked once more of another member, when
//! the pool has one. A lead may name the member a task goes to ([Ask::task_on]).

use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::Value;
use swiss_llm::agent::Effort;
use swiss_llm::LlmState;
use swiss_memory::sync::WorkerTally;
use tokio::sync::Semaphore;

use crate::memory_sync::{runner, Ask};

/// One worker a lead may name for a task: its agent's name, its runner's label and model,
/// and how many tasks it takes at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worker {
    pub name: String,
    pub label: String,
    pub model: Option<String>,
    pub seats: usize,
}

struct Member {
    name: String,
    ask: Box<dyn Ask>,
    seats: Semaphore,
    capacity: usize,
    /// Questions seated here and not ended: holding a seat or waiting for one.
    out: AtomicUsize,
    asked: AtomicU32,
    failed: AtomicU32,
}

/// A run's workers.
pub struct Pool {
    members: Vec<Member>,
}

/// One question, as any member is asked it.
#[derive(Clone, Copy)]
enum Question<'a> {
    Ask(&'a Path, &'a str),
    Fill(&'a Path, &'a str, &'a Value),
    Look(&'a Path, &'a str, &'a Value),
    Task(&'a str, Option<Effort>),
}

impl Question<'_> {
    async fn to(self, ask: &dyn Ask) -> Result<String, String> {
        match self {
            Question::Ask(root, prompt) => ask.ask(root, prompt).await,
            Question::Fill(root, prompt, schema) => ask.fill(root, prompt, schema).await,
            Question::Look(root, prompt, schema) => ask.look(root, prompt, schema).await,
            Question::Task(prompt, effort) => ask.task(prompt, effort).await,
        }
    }

    /// Whether `member` can take it: a task only a runner that takes tasks.
    fn fits(self, member: &Member) -> bool {
        !matches!(self, Question::Task(..)) || member.ask.takes_tasks()
    }
}

/// A member's `out` counted down however its question ends - answered, failed or dropped.
struct Seated<'a>(&'a AtomicUsize);

impl Drop for Seated<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Pool {
    /// The pool of named runners, in the order given; Err names the member that cannot be
    /// one ([runner]'s sentence).
    pub fn of(llm: &LlmState, names: &[String]) -> Result<Pool, String> {
        let mut members = Vec::new();
        for name in names {
            let ask = runner(llm, name).map_err(|why| format!("agent {name:?}: {why}"))?;
            members.push((name.clone(), ask));
        }
        Pool::new(members)
    }

    /// The pool of these runners; it holds at least one.
    pub fn new(members: Vec<(String, Box<dyn Ask>)>) -> Result<Pool, String> {
        if members.is_empty() {
            return Err("a pool holds at least one worker".into());
        }
        let members = members
            .into_iter()
            .map(|(name, ask)| {
                let capacity = ask.fills_at_once().max(1);
                Member {
                    name,
                    ask,
                    seats: Semaphore::new(capacity),
                    capacity,
                    out: AtomicUsize::new(0),
                    asked: AtomicU32::new(0),
                    failed: AtomicU32::new(0),
                }
            })
            .collect();
        Ok(Pool { members })
    }

    /// How many members the pool holds; always at least one, so no `is_empty`.
    pub fn member_count(&self) -> usize {
        self.members.len()
    }

    /// How each member went, in the pool's order.
    pub fn tally(&self) -> Vec<WorkerTally> {
        self.members
            .iter()
            .map(|m| WorkerTally {
                label: m.ask.label(),
                model: m.ask.model(),
                asked: m.asked.load(Ordering::SeqCst),
                failed: m.failed.load(Ordering::SeqCst),
            })
            .collect()
    }

    /// The member with the most room among those `question` fits, `skip` aside, seated at
    /// once - read and counted in one step, so two questions never both see the same
    /// free seat.
    fn seat(&self, question: Question<'_>, skip: Option<usize>) -> Option<(usize, Seated<'_>)> {
        let mut best: Option<(usize, i64, i64)> = None;
        for (at, member) in self.members.iter().enumerate() {
            if Some(at) == skip || !question.fits(member) {
                continue;
            }
            let capacity = member.capacity as i64;
            let free = capacity - member.out.load(Ordering::SeqCst) as i64;
            // free/capacity against the best's, cross-multiplied: a strict win moves it.
            let wins = match best {
                None => true,
                Some((_, best_free, best_capacity)) => free * best_capacity > best_free * capacity,
            };
            if wins {
                best = Some((at, free, capacity));
            }
        }
        let (at, _, _) = best?;
        let member = &self.members[at];
        member.out.fetch_add(1, Ordering::SeqCst);
        Some((at, Seated(&member.out)))
    }

    /// The member named `name` that `question` fits, seated.
    fn seat_named(&self, question: Question<'_>, name: &str) -> Result<(usize, Seated<'_>), String> {
        let at = self
            .members
            .iter()
            .position(|m| m.name == name && question.fits(m))
            .ok_or_else(|| format!("no worker named {name:?} takes tasks this run"))?;
        let member = &self.members[at];
        member.out.fetch_add(1, Ordering::SeqCst);
        Ok((at, Seated(&member.out)))
    }

    /// Ask the seated member, once it has a free seat; the tally counts it.
    async fn on(&self, at: usize, seated: Seated<'_>, question: Question<'_>) -> Result<String, String> {
        let member = &self.members[at];
        member.asked.fetch_add(1, Ordering::SeqCst);
        let answer = match member.seats.acquire().await {
            Ok(_permit) => question.to(member.ask.as_ref()).await,
            Err(_) => Err(format!("{}: the pool closed", member.ask.label())),
        };
        drop(seated);
        if answer.is_err() {
            member.failed.fetch_add(1, Ordering::SeqCst);
        }
        answer
    }

    /// `question` asked of the freest member - or of the named one - and, failed, once
    /// more of the freest other member.
    async fn route(&self, question: Question<'_>, named: Option<&str>) -> Result<String, String> {
        let (first, seated) = match named {
            Some(name) => self.seat_named(question, name)?,
            None => self.seat(question, None).ok_or("no worker this run takes tasks")?,
        };
        let err = match self.on(first, seated, question).await {
            Ok(answer) => return Ok(answer),
            Err(err) => err,
        };
        let Some((second, seated)) = self.seat(question, Some(first)) else {
            return Err(err);
        };
        let failed = self.members[first].ask.label();
        let retried = self.members[second].ask.label();
        self.on(second, seated, question)
            .await
            .map_err(|again| format!("{failed}: {err}; asked again of {retried}: {again}"))
    }
}

#[async_trait]
impl Ask for Pool {
    async fn ask(&self, root: &Path, prompt: &str) -> Result<String, String> {
        self.route(Question::Ask(root, prompt), None).await
    }

    async fn fill(&self, root: &Path, prompt: &str, schema: &Value) -> Result<String, String> {
        self.route(Question::Fill(root, prompt, schema), None).await
    }

    /// Every member's seats: the run keeps them all busy, each member at its own pace.
    fn fills_at_once(&self) -> usize {
        self.members.iter().map(|m| m.capacity).sum()
    }

    /// The smallest member's: a question may go to any of them.
    fn context_bytes(&self) -> Option<usize> {
        self.members.iter().filter_map(|m| m.ask.context_bytes()).min()
    }

    fn takes_tasks(&self) -> bool {
        self.members.iter().any(|m| m.ask.takes_tasks())
    }

    async fn task(&self, prompt: &str, effort: Option<Effort>) -> Result<String, String> {
        self.route(Question::Task(prompt, effort), None).await
    }

    fn roster(&self) -> Vec<Worker> {
        self.members
            .iter()
            .filter(|m| m.ask.takes_tasks())
            .map(|m| Worker { name: m.name.clone(), label: m.ask.label(), model: m.ask.model(), seats: m.capacity })
            .collect()
    }

    async fn task_on(&self, agent: Option<&str>, prompt: &str, effort: Option<Effort>) -> Result<String, String> {
        self.route(Question::Task(prompt, effort), agent).await
    }

    async fn look(&self, root: &Path, prompt: &str, schema: &Value) -> Result<String, String> {
        self.route(Question::Look(root, prompt, schema), None).await
    }

    /// Only when every member looks: a recheck then shows less code, and a member that
    /// cannot open the rest would judge blind.
    fn looks(&self) -> bool {
        self.members.iter().all(|m| m.ask.looks())
    }

    /// One member's own label; more, theirs joined with `+`.
    fn label(&self) -> String {
        self.members.iter().map(|m| m.ask.label()).collect::<Vec<_>>().join("+")
    }

    /// One member's own model; more, the distinct ones joined with `+`.
    fn model(&self) -> Option<String> {
        let mut models: Vec<String> = Vec::new();
        for model in self.members.iter().filter_map(|m| m.ask.model()) {
            if !models.contains(&model) {
                models.push(model);
            }
        }
        (!models.is_empty()).then(|| models.join("+"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    /// A worker that answers its name after `delay_ms`, failing every question when `fails`;
    /// it records the most questions it held at once.
    struct Slow {
        name: &'static str,
        seats: usize,
        delay_ms: u64,
        fails: bool,
        tasks: bool,
        now: Arc<AtomicUsize>,
        most: Arc<AtomicUsize>,
    }

    fn slow(name: &'static str, seats: usize, delay_ms: u64, fails: bool) -> Slow {
        Slow { name, seats, delay_ms, fails, tasks: true, now: Arc::default(), most: Arc::default() }
    }

    impl Slow {
        async fn answer(&self) -> Result<String, String> {
            let held = self.now.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(held, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            self.now.fetch_sub(1, Ordering::SeqCst);
            if self.fails {
                Err(format!("{} is down", self.name))
            } else {
                Ok(self.name.to_string())
            }
        }
    }

    #[async_trait]
    impl Ask for Slow {
        async fn ask(&self, _root: &Path, _prompt: &str) -> Result<String, String> {
            self.answer().await
        }
        async fn fill(&self, _root: &Path, _prompt: &str, _schema: &Value) -> Result<String, String> {
            self.answer().await
        }
        fn fills_at_once(&self) -> usize {
            self.seats
        }
        fn takes_tasks(&self) -> bool {
            self.tasks
        }
        async fn task(&self, _prompt: &str, _effort: Option<Effort>) -> Result<String, String> {
            self.answer().await
        }
        fn label(&self) -> String {
            format!("builtin:{}", self.name)
        }
        fn model(&self) -> Option<String> {
            Some(format!("m-{}", self.name))
        }
    }

    fn pool_of(members: Vec<Slow>) -> Pool {
        Pool::new(members.into_iter().map(|m| (m.name.to_string(), Box::new(m) as Box<dyn Ask>)).collect()).expect("pool")
    }

    #[tokio::test]
    async fn questions_spread_by_room_and_no_member_exceeds_its_seats() {
        let a = slow("a", 2, 30, false);
        let b = slow("b", 1, 30, false);
        let (most_a, most_b) = (a.most.clone(), b.most.clone());
        let pool = pool_of(vec![a, b]);
        assert_eq!(pool.fills_at_once(), 3);
        let root = Path::new(".");
        // Nine questions, more than the seats: every one is answered, none over a seat.
        let answers = futures_util::future::join_all((0..9).map(|_| pool.fill(root, "q", &Value::Null))).await;
        assert!(answers.iter().all(|a| a.is_ok()), "{answers:?}");
        assert!(most_a.load(Ordering::SeqCst) <= 2 && most_b.load(Ordering::SeqCst) <= 1);
        let tally = pool.tally();
        assert_eq!(tally.iter().map(|t| t.asked).sum::<u32>(), 9);
        assert!(tally.iter().all(|t| t.asked >= 2 && t.failed == 0), "both took a share: {tally:?}");
        assert_eq!(tally[0].label, "builtin:a");
        assert_eq!((pool.label(), pool.model()), ("builtin:a+builtin:b".into(), Some("m-a+m-b".into())));
    }

    #[tokio::test]
    async fn a_failed_question_is_asked_once_more_of_another_member() {
        let pool = pool_of(vec![slow("down", 4, 1, true), slow("up", 1, 1, false)]);
        // Both wholly free: the first listed is asked first.
        assert_eq!(pool.fill(Path::new("."), "q", &Value::Null).await.as_deref(), Ok("up"));
        let tally = pool.tally();
        assert_eq!((tally[0].asked, tally[0].failed, tally[1].asked, tally[1].failed), (1, 1, 1, 0));
        // Both down: the sentence names both tries, and the second is the last.
        let both = pool_of_two_down().fill(Path::new("."), "q", &Value::Null).await.expect_err("both down");
        assert!(both.contains("x is down") && both.contains("asked again of builtin:y: y is down"), "{both}");
        // A lone member has no other to ask.
        let lone = pool_of(vec![slow("solo", 1, 1, true)]);
        assert_eq!(lone.fill(Path::new("."), "q", &Value::Null).await, Err("solo is down".into()));
        assert_eq!((lone.label(), lone.model()), ("builtin:solo".into(), Some("m-solo".into())));
    }

    fn pool_of_two_down() -> Pool {
        pool_of(vec![slow("x", 1, 1, true), slow("y", 1, 1, true)])
    }

    #[tokio::test]
    async fn a_task_goes_to_the_member_named_and_only_to_those_that_take_tasks() {
        let mut spawned = slow("cc", 4, 1, false);
        spawned.tasks = false;
        let pool = pool_of(vec![spawned, slow("glm", 1, 1, false), slow("big", 1, 1, false)]);
        let roster: Vec<String> = pool.roster().into_iter().map(|w| w.name).collect();
        assert_eq!(roster, ["glm", "big"], "a runtime that takes no task is off the roster");
        assert_eq!(pool.task_on(Some("big"), "t", None).await.as_deref(), Ok("big"));
        assert_eq!(pool.task("t", None).await.as_deref(), Ok("glm"), "the first listed of the freest");
        assert!(pool.task_on(Some("cc"), "t", None).await.expect_err("takes none").contains("no worker named \"cc\""));
        assert_eq!(pool.tally()[0].asked, 0, "the spawned member was never handed a task");
    }
}
