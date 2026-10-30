// Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0.

//! The login Keychain, through the OS API. Creation never updates an existing master key.

use security_framework::os::macos::keychain::SecKeychain;
use security_framework::passwords::{generic_password, PasswordOptions};

pub fn keychain_read(service: &str, account: &str) -> Result<Option<Vec<u8>>, String> {
    match generic_password(PasswordOptions::new_generic_password(service, account)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.code() == -25300 => Ok(None), // errSecItemNotFound
        Err(error) => Err(format!("cannot read the login Keychain: {error}")),
    }
}

pub fn keychain_create(service: &str, account: &str, key: &[u8]) -> Result<bool, String> {
    let keychain = SecKeychain::default().map_err(|error| format!("cannot open the login Keychain: {error}"))?;
    match keychain.add_generic_password(service, account, key) {
        Ok(()) => Ok(true),
        Err(error) if error.code() == -25299 => Ok(false), // errSecDuplicateItem: another starter won.
        Err(error) => Err(format!("cannot save the master key in the login Keychain: {error}")),
    }
}
