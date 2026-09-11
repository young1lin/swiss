//! `lmg` — one binary, two faces: the CLI (start/stop/status/…) and `lmg serve`, the gateway
//! itself. Node had bin.ts + index.ts; a single release binary dispatches on the first
// argument instead.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // The gateway is a daemon; its environment must be this machine's, not whatever shell ran
    // the command (docs/16 §1) — the 2026-09-11 incident was NO_COLOR=1 from an agent tool
    // shell reaching every local pwsh through the gateway. Applies to serve (19998 runs
    // exactly this way) and start -f alike, both of which run the gateway in this process.
    // SAFETY: the first statement of the process — the current_thread runtime has no worker
    // threads and nothing has been spawned yet, so no other thread can observe the removal.
    unsafe { lmg_core::env::scrub_process_env() };
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("serve") {
        // The gateway — what the daemon spawns and what `lmg start -f` runs in-process.
        if let Err(err) = lmg::server::run_gateway().await {
            lmg_core::log::error("fatal", Some(serde_json::json!({ "err": err })));
            std::process::exit(1);
        }
        return;
    }
    let code = lmg::cli::main(args).await;
    std::process::exit(code);
}
