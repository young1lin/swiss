//! The listen port and its resolution order — port of `port.ts`.

/// The port the gateway listens on when nothing else names one.
pub const DEFAULT_PORT: u16 = 19999;

/// A usable TCP listen port out of a raw string, or None. Digit strings only (env vars arrive as
/// text): whitespace is trimmed first, then strict digits + the 1..=65535 range.
pub fn as_listen_port(raw: &str) -> Option<u16> {
    let s = raw.trim();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u16>().ok().filter(|p| *p >= 1)
}

/// A listen port out of a config value: JSON numbers or digit strings.
pub fn as_listen_port_value(v: &serde_json::Value) -> Option<u16> {
    match v {
        serde_json::Value::Number(n) => n
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p >= 1),
        serde_json::Value::String(s) => as_listen_port(s),
        _ => None,
    }
}

/// `MCP_GATEWAY_PORT` when it is set to a usable port. Empty/unset is None, not an error.
pub fn env_listen_port() -> Option<u16> {
    match std::env::var("MCP_GATEWAY_PORT") {
        Ok(raw) if !raw.is_empty() => as_listen_port(&raw),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_parse_strictly() {
        assert_eq!(as_listen_port("19999"), Some(19999));
        assert_eq!(as_listen_port(" 8080 "), Some(8080));
        assert_eq!(as_listen_port("0"), None);
        assert_eq!(as_listen_port("65536"), None);
        assert_eq!(as_listen_port("0x10"), None);
        assert_eq!(as_listen_port("1e2"), None);
        assert_eq!(as_listen_port(""), None);
    }
}
