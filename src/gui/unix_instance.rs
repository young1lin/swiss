// Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0.

//! SPEC §host.gui: one window per home and port, with a private local channel to raise it.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

pub enum Claim { Owned(Guard), Existing }

pub struct Guard {
    listener: UnixListener,
    _lock: File,
    path: PathBuf,
}

impl Drop for Guard {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.path); }
}

pub fn claim(home: &Path, id: &str) -> io::Result<Claim> {
    let path = home.join(format!("gui-{id}.sock"));
    let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .mode(0o600).open(home.join(format!("gui-{id}.lock")))?;
    match lock.try_lock() {
        Ok(()) => {},
        Err(TryLockError::WouldBlock) => {
            match UnixStream::connect(&path) {
                Ok(mut stream) => stream.write_all(b"show")?,
                // The owner can still be between acquiring the lock and binding its socket.
                Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {},
                Err(error) => return Err(error),
            }
            return Ok(Claim::Existing);
        }
        Err(TryLockError::Error(error)) => return Err(error),
    }
    // Only the lock owner can remove a socket left behind by an interrupted process.
    match std::fs::remove_file(&path) {
        Ok(()) => {},
        Err(error) if error.kind() == io::ErrorKind::NotFound => {},
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(&path)?;
    Ok(Claim::Owned(Guard { listener, _lock: lock, path }))
}

impl Guard {
    #[cfg(target_os = "macos")]
    pub fn listen(self, mut show: impl FnMut()) {
        for incoming in self.listener.incoming() {
            // One failed accept — a peer gone between connect and accept, EMFILE under pressure —
            // must not end the window's only raise-window receiver: `incoming()` never returns
            // None, so leaving the loop here means this window can never be brought forward
            // again. The pause keeps a persistent error from spinning a core.
            let mut stream = match incoming {
                Ok(stream) => stream,
                Err(_) => { std::thread::sleep(std::time::Duration::from_millis(50)); continue }
            };
            // A broken local client never holds the only raise-window receiver forever.
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(1)));
            let mut message = [0u8; 4];
            if stream.read_exact(&mut message).is_ok() && &message == b"show" { show(); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_identity_raises_the_owner_and_a_second_identity_is_independent() {
        let home = std::env::temp_dir().join(format!("swiss-gui-claim-{}", std::process::id()));
        std::fs::create_dir_all(&home).expect("test home");
        let Claim::Owned(owner) = claim(&home, "one").expect("first") else { panic!("first owns"); };
        assert!(matches!(claim(&home, "one").expect("second"), Claim::Existing));
        let (mut stream, _) = owner.listener.accept().expect("raise request");
        let mut message = [0u8; 4];
        stream.read_exact(&mut message).expect("message");
        assert_eq!(&message, b"show");
        let Claim::Owned(other) = claim(&home, "two").expect("other") else { panic!("independent"); };
        drop(other); drop(owner);
        let Claim::Owned(reopened) = claim(&home, "one").expect("reopen") else { panic!("claim released"); };
        drop(reopened);
        std::fs::remove_dir_all(home).expect("cleanup");
    }

    #[test]
    fn an_abandoned_socket_is_recovered_only_after_claiming_its_lock() {
        let home = std::env::temp_dir().join(format!("swiss-gui-stale-{}", std::process::id()));
        std::fs::create_dir_all(&home).expect("test home");
        drop(UnixListener::bind(home.join("gui-stale.sock")).expect("old socket"));
        let Claim::Owned(owner) = claim(&home, "stale").expect("recovered") else { panic!("new owner"); };
        drop(owner);
        std::fs::remove_dir_all(home).expect("cleanup");
    }
}
