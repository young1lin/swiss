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

//! The run recorder: one conversation noted turn by turn and filed at its end, the context
//! a run rides in, and the cancel flag a caller stops it with.

use super::*;

/// One conversation being noted turn by turn. Created from a history (or
/// [RunRecorder::none] when there is none), it accumulates notes in memory - a run is
/// bounded by its request context and its file reads - and files the run row and every turn
/// in one queued transaction at [finish](RunRecorder::finish).
pub struct RunRecorder {
    pub(super) history: Option<Arc<LlmHistory>>,
    pub(super) agent: String,
    pub(super) runtime: String,
    pub(super) provider: Option<String>,
    pub(super) model: Option<String>,
    pub(super) started: Instant,
    /// When the conversation began - the window [begun, at_end] whose same-agent
    /// calls the finish transaction stamps with the run id (the session id).
    pub(super) begun: String,
    /// The turns so far, shared with the live registry while the run runs.
    pub(super) notes: Notes,
    /// The run's lifecycle: Some(1) a Test conversation (born open, continuable),
    /// NULL a one-shot (legacy shape, hidden while pending).
    pub(super) kind: Option<i64>,
    /// The row id the reservation allocated. Calls associate by it explicitly;
    /// NULL keeps the legacy path where the finish transaction joins the window.
    pub(super) id: Option<i64>,
    /// Why the reservation refused (admission, the one-open rule, a lost reply):
    /// the consumer fails closed before any provider ask, and the route names it.
    pub(super) refusal: Option<String>,
    /// The in-flight activity attempt (SPEC llm.history): begin_round commits it
    /// durably before an outbound, the round's sealed completion clears it by
    /// token. None while no outbound is in flight.
    pub(super) attempt: Option<u64>,
    /// The STALE-PRIOR basis: the run's activity version as THIS recorder
    /// observed it - at its own reservation (a fresh row answers 0) or at its
    /// resume read. begin_round claims only against this value, never the
    /// row's current one: a competing writer's settled round moves the
    /// version, and a recorder whose prior predates it must be refused, not
    /// silently re-blessed. Observation reads never refresh it; only the
    /// recorder's own proven claim does.
    pub(super) basis: Option<i64>,
    /// The route's cancellation, when the dialogue is watched live.
    pub(super) cancel: Option<Arc<CancelFlag>>,
    /// The reader's words sent mid-run, when the dialogue is watched live.
    pub(super) steer: Option<Arc<Steer>>,
    /// What the run is for and who started it, as the consumer's scope said
    /// when the recorder was made (see [within]).
    pub(super) context: RunContext,
}

/// What a run is for and who started it (SPEC llm.history): the consumer names
/// the task and its subject around the ask, the source names the job that
/// triggered it, and a tool call names itself the parent of every run its tool
/// starts. The recorder captures it once, at creation, so the row says what the
/// scope said when the run began.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunContext {
    /// A short task key: memory.recheck, memory.task, ...
    pub task: Option<String>,
    /// What the task is about: a domain, a file, a question's first line.
    pub subject: Option<String>,
    /// The job that triggered the run: memory:<sync run id>, ...
    pub source: Option<String>,
    /// The run whose tool call started this one.
    pub parent: Option<i64>,
}

/// The longest task, subject and source a row keeps, in characters.
pub(super) const TASK_MAX: usize = 64;
pub(super) const SUBJECT_MAX: usize = 200;
pub(super) const SOURCE_MAX: usize = 128;

impl RunContext {
    /// The fields as a row keeps them: trimmed, clipped, an empty one absent.
    pub(super) fn clipped(&self) -> RunContext {
        let clip = |text: &Option<String>, max: usize| {
            text.as_deref()
                .map(|t| t.trim().chars().take(max).collect::<String>())
                .filter(|t| !t.is_empty())
        };
        RunContext {
            task: clip(&self.task, TASK_MAX),
            subject: clip(&self.subject, SUBJECT_MAX),
            source: clip(&self.source, SOURCE_MAX),
            parent: self.parent,
        }
    }
}

tokio::task_local! {
    static CONTEXT: RunContext;
}

/// Run `work` with `context` as the scope every run recorder made inside it
/// captures. The scope replaces the outer one whole: a caller that keeps a
/// field says so (`RunContext { task, ..context() }`).
pub async fn within<F: std::future::Future>(context: RunContext, work: F) -> F::Output {
    CONTEXT.scope(context, work).await
}

/// The scope in force (see [within]); outside any scope, an empty one.
pub fn context() -> RunContext {
    CONTEXT.try_with(RunContext::clone).unwrap_or_default()
}

pub(super) struct Note {
    pub(super) at: String,
    pub(super) role: String,
    pub(super) text: String,
}

/// A cancellation the route owns and the loop observes between rounds. A watch
/// channel, not a bare Notify: the state outlives the wake, so a request that
/// lands between the loop's flag check and its next await is still seen.
pub struct CancelFlag {
    pub(super) requested: tokio::sync::watch::Sender<bool>,
    pub(super) seen: tokio::sync::watch::Receiver<bool>,
}

impl CancelFlag {
    pub fn new() -> Arc<CancelFlag> {
        let (requested, seen) = tokio::sync::watch::channel(false);
        Arc::new(CancelFlag { requested, seen })
    }

    /// Request the cancel; every later wait returns at once.
    pub fn request(&self) {
        let _ = self.requested.send(true);
    }

    /// Whether the cancel was requested.
    pub fn requested(&self) -> bool {
        *self.seen.borrow()
    }

    /// Wait until requested (returns immediately when it already is).
    pub async fn wait(&self) {
        let mut seen = self.seen.clone();
        while !*seen.borrow_and_update() {
            if seen.changed().await.is_err() {
                break;
            }
        }
    }
}

/// The most words a live conversation holds waiting for its next round.
pub const STEER_MAX: usize = 16;

/// What the reader sent while the loop ran (SPEC llm.agents Test): the route posts each
/// message under the reader's own id, the loop takes them all at its next round
/// boundary - after a round's tool answers, before the next ask - and the model reads
/// them there. A message the loop never reached (the closing round was already in
/// flight) stays here until the route closes the inbox; the reader then sends it as
/// the next turn. A closed inbox refuses posts, so a late one is never silently lost.
#[derive(Default)]
pub struct Steer {
    waiting: std::sync::Mutex<SteerQueue>,
}

#[derive(Default)]
struct SteerQueue {
    items: Vec<(String, String)>,
    closed: bool,
}

/// Why a post did not land in the inbox.
#[derive(Debug, PartialEq, Eq)]
pub enum SteerRefused {
    /// The loop has ended: the reader sends the words as the next turn.
    Closed,
    /// [STEER_MAX] messages already wait.
    Full,
}

impl Steer {
    pub fn new() -> Arc<Steer> {
        Arc::new(Steer::default())
    }

    /// Queue one message for the next round boundary.
    pub fn post(&self, id: &str, text: &str) -> Result<(), SteerRefused> {
        let Ok(mut queue) = self.waiting.lock() else { return Err(SteerRefused::Closed) };
        if queue.closed {
            return Err(SteerRefused::Closed);
        }
        if queue.items.len() >= STEER_MAX {
            return Err(SteerRefused::Full);
        }
        queue.items.push((id.to_string(), text.to_string()));
        Ok(())
    }

    /// Take a waiting message back; false when the loop already took it (or it was never here).
    pub fn withdraw(&self, id: &str) -> bool {
        let Ok(mut queue) = self.waiting.lock() else { return false };
        let before = queue.items.len();
        queue.items.retain(|(one, _)| one != id);
        queue.items.len() != before
    }

    /// Everything waiting, in the order it came, as (id, text): the loop's take.
    pub fn take(&self) -> Vec<(String, String)> {
        self.waiting.lock().map(|mut queue| std::mem::take(&mut queue.items)).unwrap_or_default()
    }

    /// The loop has ended: refuse every later post and hand back what no round took.
    pub fn close(&self) -> Vec<(String, String)> {
        let Ok(mut queue) = self.waiting.lock() else { return Vec::new() };
        queue.closed = true;
        std::mem::take(&mut queue.items)
    }
}

/// The loop's cancelled-step signal (see [RunRecorder::cancelled_select]).
pub struct Cancelled;

/// Why a conversation could not be reopened (the route maps each to its 409 code).
/// Debug is derived, not hidden: tests and failure output name the exact refusal
/// (Gone vs Closed vs ContextChanged), which IS the diagnostic.
#[derive(Debug)]
pub enum ResumeError {
    /// No open conversation with that id from this seat - also the answer for
    /// another agent's run, which reads as absent, never as foreign detail.
    Gone,
    /// A one-shot run, a closed conversation, or a finished one.
    Closed,
    /// The provider or model in force changed since the run opened.
    ContextChanged,
    /// Evidenced activity without canonical proof: the legacy-ambiguous
    /// activity mark, a pruned or pre-protocol last body. The resume itself
    /// refuses - no blank context may ride the old id.
    NoCanonical,
    /// A round of the conversation is still in flight: the drive that marked
    /// it lives. The reader waits for it (or stops it) and sends again.
    Busy,
}

impl RunRecorder {
    /// The recorder for a history-less ask: every note a no-op, the finish a no-op.
    pub fn none() -> RunRecorder {
        RunRecorder {
            history: None,
            agent: String::new(),
            runtime: String::new(),
            provider: None,
            model: None,
            started: Instant::now(),
            begun: swiss_core::log::iso_now(),
            notes: Notes::default(),
            kind: None,
            id: None,
            refusal: None,
            attempt: None,
            basis: None,
            cancel: None,
            steer: None,
            context: RunContext::default(),
        }
    }

    /// One turn of the dialogue: the task (user), a model answer with its tool calls
    /// (assistant), a tool answer fed back (tool), the closing line (final) or the
    /// failure text (error).
    pub fn note(&mut self, role: &str, text: &str) {
        if self.history.is_some() {
            if let Ok(mut notes) = self.notes.lock() {
                notes.push(Note { at: swiss_core::log::iso_now(), role: role.into(), text: text.into() });
            }
        }
    }

    /// The reserved id, when the run holds one - calls and turns associate by it.
    pub fn id(&self) -> Option<i64> {
        self.id
    }

    /// Attach the route's cancellation (a conversation watched live): the loop
    /// reads it between rounds and races every provider stream against it.
    pub fn set_cancel(&mut self, cancel: Arc<CancelFlag>) {
        self.cancel = Some(cancel);
    }

    /// Whether the attached cancellation was requested.
    pub fn cancel_requested(&self) -> bool {
        self.cancel.as_ref().is_some_and(|flag| flag.requested())
    }

    /// Attach the route's inbox (a conversation watched live): the loop takes what
    /// waits there at each round boundary.
    pub fn set_steer(&mut self, steer: Arc<Steer>) {
        self.steer = Some(steer);
    }

    /// What the reader sent since the last boundary, as (id, text); nothing without an inbox.
    pub fn take_steered(&self) -> Vec<(String, String)> {
        self.steer.as_ref().map(|steer| steer.take()).unwrap_or_default()
    }

    /// Race one loop step against the cancel. A requested cancel drops the step
    /// where it stood (a provider stream mid-flight); its round never completes.
    pub async fn cancelled_select<F: std::future::Future>(&self, step: F) -> Result<F::Output, Cancelled> {
        let Some(flag) = self.cancel.clone() else {
            return Ok(step.await);
        };
        if flag.requested() {
            return Err(Cancelled);
        }
        tokio::select! {
            biased;
            _ = flag.wait() => Err(Cancelled),
            out = step => Ok(out),
        }
    }
    /// Why the run refused to reserve, when it did: the consumer fails closed
    /// before any provider ask (admission, the one-open rule, a lost proof).
    pub fn refusal(&self) -> Option<&str> {
        self.refusal.as_deref()
    }

    /// Commit this round's ADMITTED activity fact durably BEFORE the outbound:
    /// a conditional, token-carrying mark that only the round's own sealed
    /// completion clears. Ok(attempt) is the receipt the completion must match;
    /// Err means no proof of the mark - a quota refusal, a competing attempt
    /// already in flight, a closed run, or a lost reply - and the caller asks
    /// nothing (SPEC llm.history: an unprovable round never flies).
    pub async fn begin_round(&mut self) -> Result<u64, String> {
        let Some(history) = self.history.clone() else { return Ok(0) };
        let Some(run_id) = self.id else { return Ok(0) };
        history.live.round(run_id);
        if self.kind != Some(1) { return Ok(0) }
        // The STALE-PRIOR bind: the claim names the activity version THIS
        // recorder observed - its reservation or its resume read - never the
        // row's current value, which a competing writer may have advanced
        // since the prior this round would fly was built. A recorder holding
        // no observed version claims nothing: an unobserved basis would
        // bless exactly the stale request the guard exists to refuse.
        let Some(basis) = self.basis else {
            return Err("the round has no observed activity version to claim on".to_string());
        };
        // TEST-ONLY lost-proof injection: see [RunRecorder::reserve]; the
        // mark commits, the authority proof never arrives.
        #[cfg(test)]
        let drop_reply = history
            .drop_reply_after_commit
            .lock()
            .map(|held| held.as_deref() == Some("activity"))
            .unwrap_or(false);
        // The mark names its holder: the door may release it once that holder is
        // provably gone (SPEC llm.history), never while it still drives.
        let holder = history.holder.clone();
        let (reply, receipt) = tokio::sync::oneshot::channel::<Result<i64, String>>();
        let submitted = history.submit(128, move |pool| {
            Box::pin(async move {
                let marked = sqlx::query(
                    "UPDATE run SET pending = 1, attempt = attempt + 1, holder = ?
                     WHERE id = ? AND kind = 1 AND ok IS NULL AND at_end IS NULL AND pending = 0 AND attempt = ?",
                )
                .bind(&holder)
                .bind(run_id)
                .bind(basis)
                .execute(&pool)
                .await;
                let attempt = match marked {
                    Ok(result) if result.rows_affected() > 0 => {
                        let row = sqlx::query("SELECT attempt FROM run WHERE id = ?")
                            .bind(run_id)
                            .fetch_one(&pool)
                            .await;
                        match row {
                            Ok(row) => row.try_get::<i64, _>("attempt").unwrap_or(0),
                            Err(e) => {
                                let _ = reply.send(Err(e.to_string()));
                                return Ok(());
                            }
                        }
                    }
                    // Zero rows: the run closed, or another attempt holds the
                    // mark - never overwrite a live attempt (the token rule).
                    Ok(_) => {
                        let _ = reply.send(Err("the run has an attempt in flight, moved past this recorder's basis, or is no longer continuable".to_string()));
                        return Ok(());
                    }
                    Err(e) => return Err(e.to_string()),
                };
                // Send LAST, after the commit: the receipt proves the mark.
                #[cfg(test)]
                if drop_reply {
                    return Ok(());
                }
                let _ = reply.send(Ok(attempt));
                Ok(())
            })
        });
        match submitted {
            Ok(()) => match receipt.await {
                Ok(Ok(attempt)) => {
                    self.attempt = Some(attempt as u64);
                    // Only the recorder's OWN proven claim advances its
                    // basis: the next round of this recorder claims against
                    // the version it itself just minted.
                    self.basis = Some(attempt);
                    Ok(attempt as u64)
                }
                Ok(Err(why)) => Err(why),
                // No reply is no proof the mark committed - and no proof it did
                // not. The round refuses to fly; a stranded mark (if any) leaves
                // the run fail-closed, which is the honest cost.
                Err(_) => Err("the activity reply was lost".to_string()),
            },
            Err(e) => Err(e.to_string()),
        }
    }

    /// The in-flight attempt the current round settles, when one was marked.
    pub fn attempt(&self) -> Option<u64> {
        self.attempt
    }

    /// Queue a sealed PARTIAL audit tail from an interrupted tool dispatch:
    /// results already observed and requests whose outcomes remain unknown.
    /// Queueing is not a commit receipt and grants no completion authority;
    /// pending stays marked. A proven settlement supplies the authoritative
    /// replay tail. Without it, the next resume releases the stale round and
    /// pairs this tail with the last sealed round's calls (see release.rs).
    pub fn file_tail(&mut self, tail: Value) {
        let Some(history) = self.history.clone() else { return };
        let Some(run_id) = self.id else { return };
        let Some(attempt) = self.attempt else { return };
        let (envelope, pieces) = split_tail(&tail);
        let envelope_text = envelope.to_string();
        // The plaintext weight prices the admission: the sealed bytes cost
        // what the decoded truth costs, not what the ciphertext compresses to.
        let weight = envelope_text.len() + pieces.iter().map(|piece| piece.text.len()).sum::<usize>();
        let estimate = weight + 256;
        let keys = history.keys.clone();
        let submitted = history.submit(estimate, move |pool| {
            let envelope_text = envelope_text.clone();
            let pieces = pieces.clone();
            Box::pin(async move {
                // One transaction: the tail's pieces seal, then the run
                // column takes the REFERENCE - hashes only, never plaintext
                // (the same machinery call bodies ride; SPEC llm.history).
                let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
                for piece in &pieces {
                    let filed = sqlx::query("INSERT OR IGNORE INTO piece (hash, chars) VALUES (?, ?)")
                        .bind(&piece.hash)
                        .bind(piece.text.chars().count() as i64)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                    if filed.rows_affected() > 0 {
                        write_piece_chunks(&mut tx, &piece.hash, &piece.text, &keys)
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                }
                sqlx::query("UPDATE run SET tail = ?, tail_plain = ? WHERE id = ? AND pending = 1 AND attempt = ?")
                    .bind(&envelope_text)
                    .bind(weight as i64)
                    .bind(run_id)
                    .bind(attempt as i64)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
                tx.commit().await.map_err(|e| e.to_string())?;
                Ok(())
            })
        });
        if let Err(e) = submitted {
            swiss_core::log::warn(&format!("llm.db tail admission refused: {e}"), None);
        }
    }

    /// The AUTHORITATIVE completion of a tool round (SPEC llm.history): the
    /// results tail - every call's answer, exact wire ids, UNKNOWN verdicts
    /// included - is sealed, and the round's activity mark cleared, in ONE
    /// transaction. Awaited: the receipt proves the completion, and only
    /// then may the next outbound be authorized. The clear is CONDITIONAL
    /// on durable proof that this attempt's canonical call landed (see
    /// the authority check inside); a completion that no longer matches
    /// the in-flight attempt (a stale finish) changes nothing; a refused
    /// admission or a failed SQL leaves the round in flight, fail-closed,
    /// with the run's rows preserved for audit.
    pub async fn settle_round(&mut self, tail: Value) -> Result<(), String> {
        let Some(history) = self.history.clone() else { return Ok(()) };
        let Some(run_id) = self.id else { return Ok(()) };
        let Some(attempt) = self.attempt else { return Ok(()) };
        let (envelope, pieces) = split_tail(&tail);
        let envelope_text = envelope.to_string();
        let weight = envelope_text.len() + pieces.iter().map(|piece| piece.text.len()).sum::<usize>();
        let estimate = weight + 256;
        let keys = history.keys.clone();
        let (reply, receipt) = tokio::sync::oneshot::channel::<Result<(), String>>();
        let submitted = history.submit(estimate, move |pool| {
            let envelope_text = envelope_text.clone();
            let pieces = pieces.clone();
            Box::pin(async move {
                let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
                // SETTLEMENT AUTHORITY (SPEC llm.history): the mark clears
                // only with durable proof that THIS attempt's canonical
                // call - the sealed Ok-with-tools body whose dispatch
                // produced these results - actually landed. The call row
                // commits only together with its sealed body chunks in
                // insert_call's one transaction, so the row's presence IS
                // the body's presence; the attempt equality keeps a prior
                // round's call (or a refused round's nothing) from ever
                // blessing a newer mark. No proof: nothing writes, nothing
                // clears - the round stays in flight and no next outbound
                // can be authorized over a body no store can produce.
                let proven = sqlx::query(
                    "SELECT 1 FROM call WHERE run_id = ? AND attempt = ? AND ok = 1 LIMIT 1",
                )
                .bind(run_id)
                .bind(attempt as i64)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
                if proven.is_none() {
                    let _ = reply.send(Err(
                        "the round's canonical call never landed: its results cannot clear the mark".to_string(),
                    ));
                    return Ok(());
                }
                for piece in &pieces {
                    let filed = sqlx::query("INSERT OR IGNORE INTO piece (hash, chars) VALUES (?, ?)")
                        .bind(&piece.hash)
                        .bind(piece.text.chars().count() as i64)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                    if filed.rows_affected() > 0 {
                        write_piece_chunks(&mut tx, &piece.hash, &piece.text, &keys)
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                }
                let settled = sqlx::query(
                    "UPDATE run SET pending = 0, tail = ?, tail_plain = ? WHERE id = ? AND pending = 1 AND attempt = ?",
                )
                .bind(&envelope_text)
                .bind(weight as i64)
                .bind(run_id)
                .bind(attempt as i64)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
                if settled.rows_affected() == 0 {
                    // The mark moved or cleared: this completion is stale.
                    // Nothing changed here; the audit rows stay untouched.
                    let _ = reply.send(Err("the round's completion no longer matches its in-flight attempt".to_string()));
                    return Ok(());
                }
                tx.commit().await.map_err(|e| e.to_string())?;
                let _ = reply.send(Ok(()));
                Ok(())
            })
        });
        match submitted {
            Ok(()) => match receipt.await {
                Ok(answer) => answer,
                Err(_) => Err("the settlement reply was lost; the round state is unproven".to_string()),
            },
            Err(e) => Err(e.to_string()),
        }
    }

    /// Reserve the run's row id - a REAL insert through the store's pool, so ids
    /// are database-allocated (two handles, two processes, a restart: never the
    /// same id twice, never an orphan's id reused). A pending one-shot row (kind
    /// NULL) stays hidden from the public lists until finish; a conversation's
    /// row is born open (kind 1). No-op when the recorder already holds an id or
    /// has no history.
    pub async fn reserve(&mut self) {
        let Some(history) = self.history.clone() else { return };
        if self.id.is_some() {
            return;
        }
        let agent = self.agent.clone();
        let runtime = self.runtime.clone();
        let provider = self.provider.clone();
        let model = self.model.clone();
        let at = self.begun.clone();
        let kind = self.kind;
        let context = self.context.clipped();
        // The reservation is an ADMITTED write, not a query-side insert: a quota
        // that refuses it refuses the run before any provider ask, and the id
        // reaches this caller only through a reply that proves the commit. A
        // lost reply (the sender can die between commit and send) carries no
        // proof - the consumer fails closed, and the stranded row is inert.
        // TEST-ONLY lost-proof injection: skipping the send AFTER the INSERT
        // committed leaves a durable row the caller cannot prove - the
        // fail-closed shape the contract pins. Compiled out of release.
        #[cfg(test)]
        let drop_reply = history
            .drop_reply_after_commit
            .lock()
            .map(|held| held.as_deref() == Some("reserve"))
            .unwrap_or(false);
        let (reply, receipt) = tokio::sync::oneshot::channel::<Result<i64, String>>();
        let submitted = history.submit(256, move |pool| {
            Box::pin(async move {
                // ONE open conversation per agent and scope, decided by the write
                // itself: the conditional INSERT is atomic across handles, processes
                // and the route's own TOCTOU window. A conversation (kind 1) births
                // only while no open run of the same agent, task and subject exists
                // (a Test has neither; a Data agent's names its connection); one-shots
                // (kind NULL) never contend.
                let inserted = if kind == Some(1) {
                    sqlx::query(
                        "INSERT INTO run (at, at_end, agent, runtime, provider, model, ok, ms, turns, kind, task, subject, source, parent)
                         SELECT ?, NULL, ?, ?, ?, ?, NULL, 0, 0, ?, ?, ?, ?, ?
                         WHERE NOT EXISTS (SELECT 1 FROM run WHERE agent = ? AND task IS ? AND subject IS ? AND kind = 1 AND ok IS NULL AND at_end IS NULL AND pending IN (0, 1))",
                    )
                    .bind(&at)
                    .bind(&agent)
                    .bind(&runtime)
                    .bind(&provider)
                    .bind(&model)
                    .bind(kind)
                    .bind(&context.task)
                    .bind(&context.subject)
                    .bind(&context.source)
                    .bind(context.parent)
                    .bind(&agent)
                    .bind(&context.task)
                    .bind(&context.subject)
                    .execute(&pool)
                    .await
                } else {
                    sqlx::query(
                        "INSERT INTO run (at, at_end, agent, runtime, provider, model, ok, ms, turns, kind, task, subject, source, parent)
                         VALUES (?, NULL, ?, ?, ?, ?, NULL, 0, 0, ?, ?, ?, ?, ?)",
                    )
                    .bind(&at)
                    .bind(&agent)
                    .bind(&runtime)
                    .bind(&provider)
                    .bind(&model)
                    .bind(kind)
                    .bind(&context.task)
                    .bind(&context.subject)
                    .bind(&context.source)
                    .bind(context.parent)
                    .execute(&pool)
                    .await
                };
                let landed = match inserted {
                    Ok(result) if result.rows_affected() > 0 => result.last_insert_rowid(),
                    Ok(_) => {
                        let _ = reply.send(Err("open-run-exists".to_string()));
                        return Ok(());
                    }
                    Err(e) => return Err(e.to_string()),
                };
                // Send LAST, after the commit: a benign send failure (the caller
                // went away) still leaves a durable row, and the caller that
                // hears nothing assumes nothing.
                #[cfg(test)]
                if drop_reply {
                    return Ok(());
                }
                let _ = reply.send(Ok(landed));
                Ok(())
            })
        });
        match submitted {
            Ok(()) => match receipt.await {
                Ok(Ok(id)) => {
                    self.id = Some(id);
                    history.live.begin(id, self.notes.clone());
                    // The row this recorder itself created answers version 0:
                    // its first round claims against exactly that.
                    self.basis = Some(0);
                }
                Ok(Err(why)) => self.refusal = Some(why),
                Err(_) => {
                    // No reply is no proof: the write may or may not have
                    // committed. Refuse to ask; the consumer fails closed.
                    self.refusal = Some("the reservation reply was lost".to_string());
                }
            },
            Err(e) => self.refusal = Some(e.to_string()),
        }
    }

    /// Let go of a recorder that asked nothing (a continuation refused after its run
    /// reopened, a guard's look): it files nothing, and its run leaves the live registry
    /// so no list shows it running.
    pub fn release(mut self) {
        if let (Some(history), Some(id)) = (self.history.take(), self.id) {
            history.live.end(id);
        }
    }

    /// End the run on a failure: the reason as its closing error turn, then the
    /// failed finish. A conversation still stays open (see [finish](Self::finish)).
    pub fn fail(&mut self, why: &str) {
        self.note("error", why);
        self.finish(false);
    }

    /// File the run: the run row (its outcome and wall time) plus every noted turn,
    /// bodies chunked and sealed, one queued transaction. Takes the recorder by
    /// &mut so the CREATOR of the run can still file it when a consumer-side
    /// deadline drops the future that was driving the dialogue (a timeout kills
    /// the loop, not the record); a second finish is a no-op.
    pub fn finish(&mut self, ok: bool) {
        let Some(history) = self.history.take() else { return };
        if let Some(id) = self.id {
            history.live.end(id);
        }
        let notes = self.notes.lock().map(|mut held| std::mem::take(&mut *held)).unwrap_or_default();
        let at = notes
            .first()
            .map(|note| note.at.clone())
            .unwrap_or_else(swiss_core::log::iso_now);
        let run = RunRow {
            at,
            at_end: swiss_core::log::iso_now(),
            agent: std::mem::take(&mut self.agent),
            runtime: std::mem::take(&mut self.runtime),
            provider: self.provider.take(),
            model: self.model.take(),
            ok,
            ms: self.started.elapsed().as_millis() as u64,
            turns: notes.len() as i64,
            begun: self.begun.clone(),
            context: self.context.clipped(),
        };
        let estimate: usize = notes.iter().map(|note| note.text.len() + 128).sum::<usize>() + 512;
        let keys = history.keys.clone();
        let reserved = self.id;
        // A conversation files its turns and stays OPEN - each send's outcome is
        // the route's done frame, not the run row's; only a reader's close (or a
        // one-shot's finish) writes at_end.
        let conversation = self.kind == Some(1);
        let submitted = history.submit(estimate, move |pool| {
            let run = run.clone();
            Box::pin(async move {
                match reserved {
                    // A reserved run finishes its OWN row in place: the id was
                    // allocated at reservation, its calls already point at it, and
                    // nothing is ever joined by a window.
                    Some(id) => finish_run(&pool, id, conversation, &run, &notes, &keys).await,
                    None => insert_run(&pool, &run, &notes, &keys).await,
                }
                .map_err(|e| e.to_string())
            })
        });
        if let Err(e) = submitted {
            swiss_core::log::warn(&format!("llm.db run admission refused: {e}"), None);
        }
    }
}
