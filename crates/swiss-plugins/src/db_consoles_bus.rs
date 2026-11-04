//! DATA.CONSOLES requests; the store owns all writes and events (SPEC §bus.requests).

use super::*;

impl From<Console> for v1::ConsoleRecord {
    fn from(c: Console) -> Self {
        Self { id: c.id, tab: c.tab, agent: c.agent, agent_tab: c.agent_tab, open: c.open,
            sql: c.sql, revision: c.revision, created_at: c.created_at, updated_at: c.updated_at,
            closed_at: c.closed_at }
    }
}

impl From<v1::ConsoleRecord> for Console {
    fn from(c: v1::ConsoleRecord) -> Self {
        Self { id: c.id, tab: c.tab, agent: c.agent, agent_tab: c.agent_tab, open: c.open,
            sql: c.sql, revision: c.revision, created_at: c.created_at, updated_at: c.updated_at,
            closed_at: c.closed_at }
    }
}

impl ConsoleStore {
    pub(super) fn handle(&self, call: Call, services: &RuntimeServices) -> Answer {
        // Every arm keeps its lease through the operation, as the HTTP store did
        // (SPEC §host.seats). Bad protobuf never reaches the sealed store.
        macro_rules! body {
            ($ty:ty) => {
                match <$ty>::decode(call.body.as_ref()) {
                    Ok(body) => body,
                    Err(_) => return Answer::fault(swiss_bus::BAD_BODY, "the request body did not decode"),
                }
            };
        }
        macro_rules! lease {
            ($conn:expr) => {
                match services.catalog.lease($conn, "db-consoles") {
                    Ok(lease) => lease,
                    Err(e) => return Answer::fault(swiss_bus::UNAVAILABLE, &e.to_string()),
                }
            };
        }
        let result = match v1::ConsolesId::try_from(call.id as i32) {
            Ok(v1::ConsolesId::ConsolesList) => {
                let body = body!(v1::ConsolesPage);
                let _lease = lease!(&body.conn);
                self.page(&body.conn, &body.after).map(|page| page.encode_to_vec())
            }
            Ok(v1::ConsolesId::ConsolesPut) => {
                let body = body!(v1::ConsolePut);
                let _lease = lease!(&body.conn);
                let Some(console) = body.console else {
                    return Answer::fault(swiss_bus::BAD_BODY, "console record is required");
                };
                let mode = match v1::SaveMode::try_from(body.mode) {
                    Ok(v1::SaveMode::Auto) => SaveMode::Auto,
                    Ok(v1::SaveMode::Create) => SaveMode::Create,
                    Ok(v1::SaveMode::Update) => SaveMode::Update,
                    Err(_) => return Answer::fault(swiss_bus::BAD_BODY, "mode must be auto, create or update"),
                };
                self.put(&body.conn, console.into(), mode, &call.page_id)
                    .map(|console| v1::ConsoleSaved { console: Some(console.into()) }.encode_to_vec())
            }
            Ok(v1::ConsolesId::ConsolesRemove) => {
                let body = body!(v1::ConsoleRemove);
                let _lease = lease!(&body.conn);
                if body.id.is_empty() { return Answer::fault(swiss_bus::BAD_BODY, "id is required"); }
                self.remove(&body.conn, &body.id, Some(body.revision), &call.page_id)
                    .map(|removed| v1::ConsoleRemoved { id: body.id, removed }.encode_to_vec())
            }
            Ok(v1::ConsolesId::ConsolesClearClosed) => {
                let body = body!(v1::ConsolesOf);
                let _lease = lease!(&body.conn);
                self.clear_closed(&body.conn, &call.page_id)
                    .map(|cleared| v1::ConsolesCleared { cleared: cleared as u32 }.encode_to_vec())
            }
            _ => return Answer::fault(swiss_bus::UNKNOWN, "no such message"),
        };
        match result {
            Ok(bytes) => Answer::ok(bytes),
            Err(e) => Answer::fault(code_of(&e).unwrap_or(""), &e),
        }
    }
}
