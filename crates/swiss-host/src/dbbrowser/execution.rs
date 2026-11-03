/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */
//! Optional SQL execution observation (SPEC §data.history). A scope belongs to one leased
//! operation. Only the adapter's submission mark admits a record; policy refusals never do.
//! The Data plugin owns the durable sink. No adapter or producer depends on that plugin.

use std::cell::RefCell;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use serde_json::Value;

pub struct SqlRun {
    pub conn: String,
    pub target: String,
    pub dialect: String,
    pub stmt: String,
    pub ok: bool,
    pub error: Option<String>,
    pub rows: Option<i64>,
    pub ms: u64,
}

pub trait SqlRecorder: Send + Sync {
    fn record(&self, run: SqlRun);
}

#[derive(Default)]
pub struct SqlHistorySeat(Mutex<Option<Arc<dyn SqlRecorder>>>);

impl SqlHistorySeat {
    pub fn install(&self, recorder: Arc<dyn SqlRecorder>) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(recorder);
    }

    pub fn clear(&self) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    pub async fn observe<E>(
        &self, conn: &str, target: String, dialect: &str,
        future: impl Future<Output = Result<Value, E>>, error: impl FnOnce(&E) -> String,
    ) -> Result<Value, E> {
        let recorder = self.0.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let Some(recorder) = recorder else { return future.await };
        let pending = Pending { recorder, conn: conn.into(), target, dialect: dialect.into(), submitted: None };
        CURRENT.scope(RefCell::new(pending), async {
            let out = future.await;
            CURRENT.with(|cell| {
                let mut p = cell.borrow_mut();
                if let Some((stmt, started)) = p.submitted.take() {
                    let rows = out.as_ref().ok().and_then(|v| v.get("affectedRows").or_else(|| v.get("rowCount"))).and_then(Value::as_i64);
                    let why = out.as_ref().err().map(error);
                    p.finish(stmt, started, out.is_ok(), why, rows);
                }
            });
            out
        }).await
    }
}

struct Pending {
    recorder: Arc<dyn SqlRecorder>,
    conn: String,
    target: String,
    dialect: String,
    submitted: Option<(String, Instant)>,
}

impl Pending {
    fn finish(&self, stmt: String, started: Instant, ok: bool, error: Option<String>, rows: Option<i64>) {
        self.recorder.record(SqlRun { conn: self.conn.clone(), target: self.target.clone(), dialect: self.dialect.clone(),
            stmt, ok, error, rows, ms: started.elapsed().as_millis() as u64 });
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some((stmt, started)) = self.submitted.take() {
            self.finish(stmt, started, false, Some("Stopped waiting after SQL submission; database completion is unknown".into()), None);
        }
    }
}

tokio::task_local! { static CURRENT: RefCell<Pending>; }

/// Called immediately before submitting the user's SQL, after validation and transaction setup.
/// Internal BEGIN/SET/ROLLBACK statements are deliberately not observations.
pub fn submitted(sql: &str) {
    let _ = CURRENT.try_with(|cell| {
        let mut p = cell.borrow_mut();
        if p.submitted.is_none() { p.submitted = Some((sql.into(), Instant::now())); }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[derive(Default)]
    struct Capture(Mutex<Vec<SqlRun>>);
    impl SqlRecorder for Capture { fn record(&self, run: SqlRun) { self.0.lock().expect("capture").push(run); } }

    #[tokio::test]
    async fn only_submitted_statements_record_once_including_failures() {
        let seat = SqlHistorySeat::default();
        let sink = Arc::new(Capture::default());
        seat.install(sink.clone());
        let call = |sql: &'static str, sent: bool, ok: bool| async move {
            if sent { submitted(sql); }
            if ok { Ok(json!({ "rowCount": 3 })) } else { Err("database refused".to_string()) }
        };
        let _ = seat.observe("a", "db @ host".into(), "pg", call("SELECT 1 LIMIT 5", true, true), Clone::clone).await;
        let _ = seat.observe("a", "db @ host".into(), "pg", call("SELECT missing", true, false), Clone::clone).await;
        let _ = seat.observe("a", "db @ host".into(), "pg", call("DROP TABLE t", false, false), Clone::clone).await;
        let rows = sink.0.lock().expect("capture");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].stmt, "SELECT 1 LIMIT 5");
        assert_eq!(rows[0].rows, Some(3));
        assert!(!rows[1].ok);
        assert_eq!(rows[1].error.as_deref(), Some("database refused"));
    }
    #[tokio::test]
    async fn dropping_a_submitted_operation_records_unknown_completion_once() {
        let seat = SqlHistorySeat::default();
        let sink = Arc::new(Capture::default());
        seat.install(sink.clone());
        let mut waiting = Box::pin(seat.observe("a", "db @ host".into(), "mysql", async {
            submitted("SELECT 1");
            submitted("internal duplicate mark");
            std::future::pending::<Result<Value, String>>().await
        }, Clone::clone));
        std::future::poll_fn(|cx| {
            assert!(waiting.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        }).await;
        drop(waiting);
        let rows = sink.0.lock().expect("capture");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].stmt, "SELECT 1");
        assert!(!rows[0].ok);
        assert!(rows[0].error.as_deref().expect("unknown completion").contains("unknown"));
    }

}
