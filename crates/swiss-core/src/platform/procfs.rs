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

// Windows builds never call these (unix.rs owns the I/O that feeds them), but the parsers are
// the contract the Linux probe implements, so they stay un-cfg'd and tested on every host -
// "dead" on a Windows non-test build is the arrangement working as intended.
#![allow(dead_code)]

//! Pure parsers for Linux /proc text formats (SPEC §process.detached): the LISTEN rows of
//! /proc/net/tcp{,6}, the starttime field of /proc/<pid>/stat, the btime line of /proc/stat and
//! the socket-inode target of an fd link.
//!
//! Parsing text is where the format's traps live (a comm with parens, a state column in hex, an
//! IPv6 local address whose colons are not separators), so every function here is pure over
//! text and un-`cfg`'d: the suite tests them on whatever host runs it, the same contract
//! `platform/mod.rs` keeps for the shared walk. Only the I/O halves - reading the files and
//! joining the rows to pids - are Linux-only, in `unix.rs`.

/// The LISTEN rows of /proc/net/tcp or /proc/net/tcp6, as (local port, socket inode).
///
/// Columns, whitespace-separated: sl, local_address, rem_address, st, tx:rx, tr:tm->when,
/// retrnsmt, uid, timeout, inode. The state is hex ("0A" is TCP_LISTEN), the port is the hex
/// after the LAST colon of local_address - an IPv6 address is 32 hex digits carrying colons of
/// its own, so splitting on the first one would read garbage.
pub(crate) fn tcp_listen_rows(text: &str) -> Vec<(u16, u64)> {
    let mut rows = Vec::new();
    // Line one is the header ("sl local_address rem_address st ...").
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 10 {
            continue; // a torn or truncated row reads as absent, never as a wrong port
        }
        let Some((_, port_hex)) = fields[1].rsplit_once(':') else {
            continue;
        };
        let Ok(port) = u16::from_str_radix(port_hex, 16) else {
            continue;
        };
        if !fields[3].eq_ignore_ascii_case("0A") {
            continue; // ESTABLISHED, TIME_WAIT, closing states - not listeners
        }
        let Ok(inode) = fields[9].parse::<u64>() else {
            continue;
        };
        rows.push((port, inode));
    }
    rows
}

/// The starttime field (field 22) of /proc/<pid>/stat, in clock ticks since boot.
///
/// Field 2 is `comm`, wrapped in parens, and it can contain BOTH spaces and parens - a process
/// named `foo) 1 2 3 (bar` is legal. Everything after the LAST ')' is the trustworthy tail;
/// within it, starttime is the 20th token (state is the 1st, ppid the 2nd - ppid_from_stat
/// counts the same tail).
pub(crate) fn stat_start_time_ticks(stat: &str) -> Option<u64> {
    let after = stat.rsplit_once(')')?.1;
    after.split_whitespace().nth(19)?.parse().ok()
}

/// The btime line of /proc/stat - seconds since the epoch the machine booted. starttime ticks
/// are relative to btime, so a creation time in Unix terms is btime + ticks / CLK_TCK.
pub(crate) fn boot_time_seconds(text: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("btime") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// The socket inode out of an fd link target like `socket:[123456]`. Anything else a link can
/// hold (a file, an anon_inode, a pipe) reads as None.
pub(crate) fn socket_inode(link: &str) -> Option<u64> {
    let rest = link.strip_prefix("socket:[")?;
    rest.strip_suffix(']')?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_net_tcp_listen_rows_parse_for_v4_and_v6() {
        // Real shapes from a live table: the v4 row carries an 8-hex-digit address, the v6 row a
        // 32-hex-digit one whose colons must not be mistaken for the port separator.
        let v4 = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n \
  0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0\n \
  1: 00000000:1538 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12346 1 0000000000000000 100 0 0 10 0\n \
  2: 0100007F:8AE2 0100007F:1F90 01 00000000:00000000 02:0000004B 00000000  1000        0 99999 2 0000000000000000 20 4 30 10 -1\n \
  3: 0100007F:DC48 0100007F:1F90 06 00000000:00000000 00:00000000 00000000  1000        0 88888 3 0000000000000000 20 4 27 10 -1\n";
        // 0x1F90 = 8080, 0x1538 = 5432; rows 2 (ESTABLISHED) and 3 (TIME_WAIT) must drop out.
        assert_eq!(tcp_listen_rows(v4), vec![(8080, 12345), (5432, 12346)]);

        let v6 = "  sl  local_address                         rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n \
  0: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 22222 1 0000000000000000 100 0 0 10 0\n";
        assert_eq!(tcp_listen_rows(v6), vec![(8080, 22222)]);

        // Degenerate inputs read as empty, never as a panic or a guessed port.
        assert!(tcp_listen_rows("").is_empty());
        assert!(tcp_listen_rows("sl local_address rem_address st\n").is_empty());
        assert!(tcp_listen_rows("  0: 0100007F:ZZZZ 00000000:0000 0A 0 0 0 0 0 0\n").is_empty());
    }

    #[test]
    fn proc_stat_start_time_survives_a_comm_with_spaces_and_parens() {
        // The ordinary shape: comm = "node", so field 22 is the 20th token after ')'.
        let plain = "42 (node) S 7 42 42 0 -1 4194304 100 0 0 0 5 3 0 0 20 0 1 0 12345678";
        assert_eq!(stat_start_time_ticks(plain), Some(12_345_678));
        // The spoof: a comm carrying its own parens, spaces and digits. Splitting on the first
        // ')' would read the tail from inside the name; the LAST ')' is the real end of comm.
        let hostile = "42 (foo) 1 2 3 (bar) S 7 42 42 0 -1 4194304 100 0 0 0 5 3 0 0 20 0 1 0 99";
        assert_eq!(stat_start_time_ticks(hostile), Some(99));
        // A truncated tail has no field 22 - None, never a number made of the wrong column.
        assert_eq!(stat_start_time_ticks("42 (node) S 7 42"), None);
        assert_eq!(stat_start_time_ticks("garbage with no parens"), None);
    }

    #[test]
    fn btime_and_socket_inode_parse_only_their_own_shapes() {
        let stat = "cpu  1 2 3 4 5 6 7 8 9 10\ncpu0 1 2 3 4 5 6 7 8 9 10\nbtime 1760000000\nprocesses 5\n";
        assert_eq!(boot_time_seconds(stat), Some(1_760_000_000));
        assert_eq!(boot_time_seconds("cpu 1 2 3\nprocesses 5\n"), None);

        assert_eq!(socket_inode("socket:[987654321]"), Some(987_654_321));
        assert_eq!(socket_inode("/dev/null"), None);
        assert_eq!(socket_inode("anon_inode:[eventpoll]"), None);
        assert_eq!(socket_inode("socket:[]"), None);
        assert_eq!(socket_inode("socket:[abc]"), None);
    }
}
