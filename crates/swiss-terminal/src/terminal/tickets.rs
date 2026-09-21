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

//! One-shot WebSocket tickets (docs/14 §3).
//!
//! A browser cannot put a header on a WebSocket handshake, so the only thing the gateway
//! can check on the upgrade is what `loopback_guard` already checks: peer IP, `Host` and
//! `Origin`. That is enough today. The ticket exists anyway, and the reason is written
//! down in the spec: it makes "may open a terminal" an explicit, separately granted
//! thing, so that the day somebody loosens the Origin check for some other endpoint, they
//! do not loosen it onto a shell.
//!
//! Three properties, all asserted below: single use, 10 seconds, bound to one session id.
//! A presented ticket is consumed whatever the verdict — a ticket that survived being
//! offered for the wrong session would be a ticket an attacker may grind against every
//! session id in the list.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// docs/14 §3. Long enough for the panel to POST a session and open the socket; far too
/// short to be worth writing down anywhere.
pub const TICKET_TTL: Duration = Duration::from_secs(10);

/// 24 hex characters = 96 bits from the same CSPRNG the gateway's tokens use. The value
/// travels in a query string (a handshake carries no body), so it is deliberately short
/// lived rather than deliberately secret-in-a-log.
const TICKET_HEX: usize = 24;

/// Why an upgrade was refused. The strings are user-facing; the HTTP layer hands them
/// back behind a 403 rather than inventing its own wording.
#[derive(Debug, PartialEq, Eq)]
pub enum TicketError {
    /// No such ticket: never minted, already used, or long expired and swept.
    Unknown,
    /// Minted, but more than [TICKET_TTL] ago.
    Expired,
    /// Valid, but for a different session.
    WrongSession,
}

impl fmt::Display for TicketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TicketError::Unknown => {
                write!(f, "no such ticket, or it has already been used")
            }
            TicketError::Expired => write!(
                f,
                "this ticket expired; tickets are valid for {} seconds",
                TICKET_TTL.as_secs()
            ),
            TicketError::WrongSession => {
                write!(f, "this ticket was issued for a different session")
            }
        }
    }
}

struct Ticket {
    session: String,
    expires: Instant,
}

/// The outstanding tickets. Small by construction: one per session open or reconnect,
/// each alive for ten seconds.
#[derive(Default)]
pub struct TicketBook {
    entries: Mutex<HashMap<String, Ticket>>,
}

impl TicketBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a ticket for one session. Sweeps the expired ones on the way through, which
    /// is the only sweep this needs: nothing else can make the map grow.
    pub fn mint(&self, session: &str) -> String {
        let ticket = swiss_core::util::random_hex(TICKET_HEX);
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, t| t.expires > now);
        entries.insert(
            ticket.clone(),
            Ticket {
                session: session.to_string(),
                expires: now + TICKET_TTL,
            },
        );
        ticket
    }

    /// Spend a ticket on one session. The entry is removed whichever way this goes — see
    /// the module note on why a rejected ticket must not stay spendable.
    pub fn redeem(&self, ticket: &str, session: &str) -> Result<(), TicketError> {
        let taken = {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            entries.remove(ticket)
        };
        let Some(entry) = taken else {
            return Err(TicketError::Unknown);
        };
        if Instant::now() > entry.expires {
            return Err(TicketError::Expired);
        }
        if entry.session != session {
            return Err(TicketError::WrongSession);
        }
        Ok(())
    }

    /// Forget every ticket for one session — its close, so a ticket minted a moment
    /// before cannot be spent on a session that no longer exists.
    pub fn forget(&self, session: &str) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, t| t.session != session);
    }

    #[cfg(test)]
    fn outstanding(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_ticket_opens_its_own_session_exactly_once() {
        let book = TicketBook::new();
        let ticket = book.mint("sess-1");
        assert_eq!(book.redeem(&ticket, "sess-1"), Ok(()));
        // Single use: the replay of a captured query string buys nothing.
        assert_eq!(book.redeem(&ticket, "sess-1"), Err(TicketError::Unknown));
    }

    #[tokio::test(start_paused = true)]
    async fn a_ticket_expires_after_ten_seconds() {
        let book = TicketBook::new();
        let ticket = book.mint("sess-1");
        tokio::time::advance(TICKET_TTL - Duration::from_millis(1)).await;
        assert_eq!(
            book.redeem(&ticket, "sess-1"),
            Ok(()),
            "still inside the ttl"
        );

        let ticket = book.mint("sess-1");
        tokio::time::advance(TICKET_TTL + Duration::from_millis(1)).await;
        assert_eq!(book.redeem(&ticket, "sess-1"), Err(TicketError::Expired));
    }

    #[tokio::test(start_paused = true)]
    async fn a_ticket_is_bound_to_the_session_it_was_minted_for() {
        let book = TicketBook::new();
        let ticket = book.mint("sess-1");
        assert_eq!(
            book.redeem(&ticket, "sess-2"),
            Err(TicketError::WrongSession)
        );
        // And offering it at the wrong door burned it: no grinding down the session list.
        assert_eq!(book.redeem(&ticket, "sess-1"), Err(TicketError::Unknown));
    }

    #[tokio::test(start_paused = true)]
    async fn expired_tickets_do_not_pile_up() {
        let book = TicketBook::new();
        for _ in 0..50 {
            book.mint("sess-1");
        }
        tokio::time::advance(TICKET_TTL * 2).await;
        book.mint("sess-1");
        assert_eq!(book.outstanding(), 1, "the mint swept the dead ones");
    }

    #[tokio::test(start_paused = true)]
    async fn closing_a_session_invalidates_its_outstanding_tickets() {
        let book = TicketBook::new();
        let mine = book.mint("sess-1");
        let other = book.mint("sess-2");
        book.forget("sess-1");
        assert_eq!(book.redeem(&mine, "sess-1"), Err(TicketError::Unknown));
        assert_eq!(book.redeem(&other, "sess-2"), Ok(()), "untouched");
    }

    #[test]
    fn the_error_text_says_which_of_the_three_rules_was_broken() {
        // These strings reach the user through a 403; a single "forbidden" would make a
        // clock skew and a replay indistinguishable in a bug report.
        assert!(TicketError::Unknown
            .to_string()
            .contains("already been used"));
        assert!(TicketError::Expired.to_string().contains("10 seconds"));
        assert!(TicketError::WrongSession
            .to_string()
            .contains("different session"));
    }
}
