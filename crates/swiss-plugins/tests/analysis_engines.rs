/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */
//! SPEC §data.analysis: real driver shape, immutable reruns and measured constraints.
//! Run with scripts/test-analysis.ps1; no production connection or business rows are read.
use std::sync::Arc;
use serde_json::{json, Value};
use swiss_host::config::ServerDef;
use swiss_host::dbbrowser::{BrowserFlavor, DbBrowser};
use swiss_plugins::analysis::{AnalysisStore, Runner};

struct BrowserRunner(Arc<dyn DbBrowser>);
#[async_trait::async_trait]
impl Runner for BrowserRunner {
    async fn run(&self, sql: &str, limit: u32) -> Result<Value, String> {
        self.0.run_read_query(sql, Some(&json!(limit))).await
    }
}

#[tokio::test]
#[ignore = "needs isolated MySQL and PostgreSQL; scripts/test-analysis.ps1 provisions them"]
async fn native_analysis_results_survive_reruns_on_mysql_and_postgres() {
    let mysql_port = std::env::var("SWISS_ANALYSIS_MYSQL_PORT").expect("fixture MySQL port").parse::<u16>().expect("port");
    let pg_url = std::env::var("SWISS_ANALYSIS_PG_URL").expect("fixture PostgreSQL URL");
    let dir = swiss_core::paths::test_home().join("analysis-engines");
    let log = Arc::new(swiss_mcp::calls::CallLog::at(dir.join("calls")));
    for def in [
        json!({ "type": "mysql", "host": "127.0.0.1", "port": mysql_port, "user": "root", "password": "${SWISS_ANALYSIS_MYSQL_PASSWORD}", "database": "analysis_fixture" }),
        json!({ "type": "pg", "url": "${SWISS_ANALYSIS_PG_URL}" }),
    ] {
        assert!(!pg_url.is_empty());
        let kind = def["type"].as_str().expect("kind").to_string();
        let adapter = swiss_mcp::adapters::make_adapter(&ServerDef(def.as_object().expect("object").clone()), "analysis-fixture", &log, None).expect("adapter");
        let Some(BrowserFlavor::Db(browser)) = adapter.browser() else { panic!("SQL browser") };
        let runner = BrowserRunner(browser);
        let store = AnalysisStore::at(dir.join(&kind));
        let sql = "SELECT 'C01' AS customer_id, 4 AS orders, CAST('9007199254740993.01' AS DECIMAL(30,2)) AS amount UNION ALL SELECT 'C02', 7, CAST('2.00' AS DECIMAL(30,2))";
        let task = store.create(&kind, &kind, "Synthetic customers", sql, json!({ "grain": "customer", "minOrders": 3 }), json!({}), "").expect("task");
        let out = store.execute(&runner, &task.id, None, None, None).await.expect("real execution");
        let first = out["result"]["id"].as_str().expect("result");
        let page = store.rows(&task.id, first, 0, 1).expect("page");
        assert_eq!(page["rows"][0]["customer_id"], "C01", "{kind}: real object rows");
        let amount = page["rows"][0]["amount"].as_str().expect("decimal travels as an exact string");
        let (whole, fraction) = amount.split_once('.').expect("decimal fraction");
        assert_eq!(whole, "9007199254740993", "{kind}: integer precision");
        assert_eq!(fraction.trim_end_matches('0'), "01", "{kind}: exact fraction, independent of driver scale");
        assert_eq!(page["more"], true);
        let checked = store.validate(&task.id, first, &json!({ "type": "unique", "columns": ["customer_id"] })).expect("check");
        assert_eq!(checked["checks"][0]["measurement"]["actual"], 0);
        assert_eq!(checked["verification"], "partial");
        let series = store.series(&task.id, first, "customer_id", &["orders".into()]).expect("series");
        assert_eq!(series["points"][0]["orders"]["sum"], 4.0);
        let revised = store.revise(&task.id, &task.current, "SELECT 'C02' AS customer_id, 7 AS orders", json!({ "minOrders": 4 }), json!({}), "threshold changed").expect("revision");
        let next = store.execute(&runner, &task.id, Some(&revised.current), None, None).await.expect("rerun");
        let second = next["result"]["id"].as_str().expect("result");
        let diff = store.compare(&task.id, first, second, &["customer_id".into()]).expect("compare");
        assert_eq!(diff["removed"], 1);
        assert_eq!(diff["shared"], 1);
        let reopened = AnalysisStore::at(dir.join(&kind));
        assert_eq!(reopened.rows(&task.id, first, 0, 50).expect("old rows")["total"], 2);
    }
    std::fs::remove_dir_all(&dir).expect("remove fixture results");
}
