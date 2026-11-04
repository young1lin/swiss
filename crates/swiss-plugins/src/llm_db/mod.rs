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

//! The Data agent (SPEC §data.agent): a `db` agent - the runtime made for this page and run
//! by nothing else - working the one connection the Data page has open, MySQL, PostgreSQL,
//! MongoDB or Redis. The conversation opens under the runtime's DB_AGENT_PROMPT, the agent's
//! own prompt after it, and a snapshot of the connection; its tools are that kind's (DbKind)
//! and reach that connection only: find and describe what holds the data, run ONE read - a
//! statement in a read-only transaction, a read command document, a read command line - whose
//! full result streams to the page as a `db` frame while the model reads its first rows or
//! documents, and stage a write in the console for the reader to run. Redis adds its streams
//! (a window newest first, the consumer groups) and its pub/sub (the channels with a
//! subscriber, and a few seconds' listening), which no key scan shows. Every kind keeps a
//! memory (llm_db_memory): the opening carries its index, and the agent reads, saves and
//! deletes notes on where the connection's data lives and how to reach it.
//!
//! Each connection keeps its own conversation: the run is filed under the task
//! `data.agent` with the connection as its subject, and the one-open rule, a continuation
//! and the open lookup all key on that scope, so two connections never share a dialogue
//! and the agent's own Test never joins one.


mod routes;
mod text;
mod tools;

pub use routes::{compact, context, conversations, open_conversation, reopen, stream};

/// The task a Data conversation files under; its subject is the connection.
pub const TASK: &str = "data.agent";
/// The lease holder name the catalog sees.
const HOLDER: &str = "data.agent";
/// Rows a run_sql fetches when the statement sets no LIMIT of its own and the page named
/// no row limit; the page sends its console's (rowLimit), so a run brings back what the
/// console would. The server's ceiling (MAX_ROW_LIMIT) holds either way.
const RUN_LIMIT: i64 = 200;
/// How many of a result's rows the model reads back, each cell clipped; the page shows
/// them all.
const ROWS_TO_MODEL: usize = 50;
const CELL_MAX: usize = 200;
/// The longest line one document takes in what the model reads of a run_mongo.
const DOC_LINE_MAX: usize = 1_000;
/// The COUNT one scan_keys page asks SCAN for first, and the keys it walks on to hold.
const KEYS_SCANNED: i64 = 200;
/// The entries one read_stream brings when the model names no count, and the most it may.
const STREAM_ENTRIES: i64 = 20;
const STREAM_ENTRIES_MAX: i64 = 100;
/// The channels the model reads by name from list_channels, and the snapshot names.
const CHANNELS_TO_MODEL: usize = 200;
const CHANNELS_NAMED: usize = 20;
/// How long listen_channels listens when the model names no time, and the longest it may: the
/// loop waits on it, and pub/sub keeps nothing, so a long wait only makes the reader wait.
const LISTEN_SECONDS: u64 = 5;
const LISTEN_SECONDS_MAX: u64 = 15;
/// The messages a listen reads when the model names no count, and the most it may.
const LISTEN_MESSAGES: i64 = 20;
const LISTEN_MESSAGES_MAX: i64 = 50;
/// How many times a listen opens its feed when the connect or the SUBSCRIBE times out: a link
/// busy with another feed's flood (a tunnel carrying a wide pattern's messages) can hold a new
/// connection past its 5 s, and the next try usually gets through.
const LISTEN_OPEN_TRIES: u32 = 3;
/// A message payload as the model reads it, in characters.
const PAYLOAD_SHOWN: usize = 300;
/// The most databases a MongoDB snapshot names.
const DATABASES_NAMED: usize = 50;
/// The ceiling on any one tool answer the model reads.
const ANSWER_TEXT_MAX: usize = 12_000;
/// The most tables the opening snapshot names: a database with more is counted there, never
/// listed - a thousand names would weigh down every request of the conversation - and
/// list_tables finds them by name. And how many list_tables answers.
const TABLES_NAMED: usize = 100;
const TABLES_LISTED: i64 = 500;
/// The longest statement or command a tool takes, and the longest prompt (a pasted query fits).
const SQL_MAX: usize = 20_000;
const PROMPT_MAX: usize = 8_000;
/// The longest focus a compaction takes, and how many conversations a History page lists.
const FOCUS_MAX: usize = 2_000;
const HISTORY_PAGE: i64 = 20;
/// The one answer to a second send on a connection the agent is already working.
const BUSY: &str = "this agent is already working on this connection";

/// What a compaction asks after the conversation it condenses (SPEC §data.agent Compact):
/// a checkpoint in fixed sections that carries a database dialogue on without its turns.
pub const DB_COMPACT_INSTRUCTION: &str = "Condense the conversation ABOVE into a checkpoint that lets you carry this dialogue on with no loss of what matters: the turns themselves will be gone, the checkpoint stays in their place.

Output EXACTLY the Markdown structure below: keep every section, in order. Use terse bullets, not prose paragraphs. Write \"(none)\" for an empty section - never drop a section.

## Requests and Intent
- [what the reader asked, first to latest; quote them where the exact wording matters]

## Database Facts
- [the tables, columns, keys, types, row counts and value shapes learned so far; the dialect quirks met]

## Statements
- [each statement that mattered, exactly as run, in a sql block, and what its result showed - counts, shapes, the conclusion; never the rows themselves]

## Errors and Fixes
- [an error a statement met, quoted exactly, and how it was fixed]

## Pending
- [what the reader asked that is not done yet]

## Current Work
- [precisely where the dialogue stood: the last question and the answer it got]

## Critical Context
- [the reader's corrections and preferences, decisions and their reasons, constraints, open questions]

Rules:
- Keep exact table and column names, statements, error strings and numbers.
- Write no row values beyond the few a later statement needs (an id, a type name).
- Write in the language the reader writes in.
- Do NOT mention this request or that the conversation was condensed.
- Output only the checkpoint text: call no tool.
- If the conversation opens with an earlier checkpoint, do not copy it forward verbatim: keep what is still true, drop what is stale and merge the newer turns into one checkpoint under the same structure.";

