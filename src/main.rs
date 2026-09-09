//! `lmg` — one binary, two faces: the CLI (start/stop/status/…) and `lmg serve`, the gateway
//! itself. Node had bin.ts + index.ts; a single release binary dispatches on the first
// argument instead.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("serve") {
        // The gateway — what the daemon spawns and what `lmg start -f` runs in-process.
        if let Err(err) = local_mcp_gateway::server::run_gateway().await {
            local_mcp_gateway::log::error("fatal", Some(serde_json::json!({ "err": err })));
            std::process::exit(1);
        }
        return;
    }
    let code = local_mcp_gateway::cli::main(args).await;
    std::process::exit(code);
}

