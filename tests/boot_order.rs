/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */

//! A losing boot cannot replace the live gateway's session or run sequence.

#[tokio::test]
async fn an_occupied_port_fails_before_shared_state_is_read_or_replaced() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve port");
    let port = listener.local_addr().expect("bound address").port();
    let home = std::env::temp_dir().join(format!("swiss-bind-order-{}", swiss_core::util::random_hex(8)));
    std::fs::create_dir_all(&home).expect("scratch home");
    // Deliberately unreadable: even attempting to load this file before binding is a bug.
    std::fs::write(home.join("session.json"), b"live-session-sentinel").expect("session sentinel");
    std::fs::write(home.join("runs.seq"), b"777").expect("sequence sentinel");
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_swiss"))
        .arg("serve").env("SWISS_HOME", &home).env("SWISS_PORT", port.to_string())
        .env("SWISS_MASTER_KEY", "ab".repeat(32)).kill_on_drop(true)
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().expect("spawn losing boot");
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait_with_output())
        .await.expect("an occupied port fails promptly").expect("child output");
    assert!(!output.status.success());
    let message = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(message.contains("listen") && message.contains("failed"), "{message}");
    assert_eq!(std::fs::read(home.join("session.json")).unwrap(), b"live-session-sentinel");
    assert_eq!(std::fs::read(home.join("runs.seq")).unwrap(), b"777");
}
