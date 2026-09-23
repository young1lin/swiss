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

//! The daemon's environment is this machine's, not its launcher's — docs/16 §1.
//!
//! `swiss start` hands the detached gateway the environment of whatever shell ran it, and the
//! gateway hands that on to every child it spawns (proc MCPs, job scripts, local shells). A
//! launcher that says "you are inside an agent / inside CI / do not draw colours" therefore
//! speaks for every process below the gateway too — the 2026-09-11 incident was a gateway
//! started from an agent tool shell carrying NO_COLOR=1, which turned every local pwsh on 19999
//! black and white via $PSStyle.OutputRendering.
//!
//! This is a BLACKLIST, deliberately short: a whitelist-style rebuild would one day delete a
//! variable nobody thought of (a proxy, a locale) on some machine we never saw. Only the
//! variables whose whole job is to describe the launcher get struck; everything else — PATH,
//! HOME, TEMP, proxies, SWISS_* — is inherited untouched.

use std::process::Command;

/// The env var swiss start uses to hand the detached serve child its launcher's
/// attribution (pid + argv, as JSON). The launcher exits immediately after the spawn by
/// design, so the environment - captured at spawn - is the only channel that outlives it;
/// the serve path reads it at boot, logs the lineage, and removes the variable before any
/// child of its own inherits it (same single-threaded-boot argument as the PATH repair).
/// NOT part of LAUNCHER_NOISE: the blacklist runs first in main() and would delete it
/// before the boot log could read it.
pub const SPAWNER_ENV: &str = "SWISS_SPAWNER";

/// Variables that describe the LAUNCHER, not the machine: an agent harness, a CI runner, or a
/// terminal emulator marking its own session. Each one is a reason a child decided it should
/// behave differently (skip colour, skip prompts) because of who started the gateway.
///
/// Observed in the wild on 2026-09-12 in an agent tool shell on this machine: NO_COLOR,
/// WT_SESSION, WT_PROFILE_ID, DSH_SESSION_ID, DSH_SHELL. The Claude Code markers come from the
/// harness this repo's history was written under; the CI markers from the runners jobs scripts
/// already key off.
pub const LAUNCHER_NOISE: &[&str] = &[
    // The no-colours convention (https://no-color.org) — the exact variable behind the
    // 2026-09-11 incident: pwsh maps it to $PSStyle.OutputRendering=PlainText.
    "NO_COLOR",
    // CI runners. Jobs run user scripts; scripts gate interactivity and colour on these.
    "CI",
    "TF_BUILD", // Azure Pipelines
    "GITHUB_ACTIONS",
    // Agent-harness markers (Claude Code sets CLAUDECODE and CLAUDE_CODE_ENTRYPOINT; the
    // prefix entry below catches the rest of that family).
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    // This machine's agent harness marks its tool shells; a daemon inheriting a session id
    // would report a session it left hours ago.
    "DSH_SESSION_ID",
    "DSH_SHELL",
    // Windows Terminal session identity — a child reading these believes it draws into a WT
    // tab. The gateway's children draw into pipes and PTYs, never a WT tab.
    "WT_SESSION",
    "WT_PROFILE_ID",
    // "Which terminal launched you" on macOS/Linux (iTerm2, VS Code, Apple_Terminal).
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
];

/// Prefixes rather than names: families whose members are open-ended. Claude Code mints new
/// CLAUDE_CODE_* variables per release, so matching the prefix keeps the list from rotting.
pub const LAUNCHER_NOISE_PREFIXES: &[&str] = &["CLAUDE_CODE_"];

/// Does this variable name belong to the launcher's noise? Case-insensitive, because Windows
/// compares environment names that way: no_color is NO_COLOR there.
pub fn is_launcher_noise(name: &str) -> bool {
    LAUNCHER_NOISE
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
        || LAUNCHER_NOISE_PREFIXES.iter().any(|prefix| {
            // `get`, not a byte-index slice: a variable name is arbitrary Unicode on Windows,
            // and this runs on every name at the top of main — a multibyte character straddling
            // the prefix length must answer "not noise", never panic the whole CLI.
            name.get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        })
}

/// The noise names actually present in THIS process's environment. The exact names cover the
/// common case; the scan is what catches the open-ended prefixes (any live CLAUDE_CODE_*).
pub fn launcher_noise_present() -> Vec<String> {
    std::env::vars_os()
        .filter(|(name, _)| is_launcher_noise(&name.to_string_lossy()))
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect()
}

/// Strike the launcher's noise from a child about to be spawned — the `swiss start` path. The
/// child of a Command inherits this process's environment unless a name is removed, so both the
/// fixed list and whatever prefixed noise is present get env_remove'd.
pub fn scrub_command(command: &mut Command) {
    for name in LAUNCHER_NOISE {
        command.env_remove(name);
    }
    for name in launcher_noise_present() {
        command.env_remove(name);
    }
}

/// Strike the same list from THIS process — the `swiss serve` path (19998 runs exactly this way,
/// so there is no spawning parent to do it for it). Also covers `start -f`, which runs the
/// gateway in-process.
///
/// # Safety
///
/// `remove_var` races any thread that reads the environment. The contract is the caller runs
/// this before any thread exists: the very top of `main`, where the current_thread runtime has
/// spawned nothing yet.
pub unsafe fn scrub_process_env() {
    let present = launcher_noise_present();
    for name in LAUNCHER_NOISE {
        std::env::remove_var(name);
    }
    for name in present {
        std::env::remove_var(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_names_the_launcher_not_the_machine() {
        // Every entry must itself count as noise — a name in the list that does not match the
        // matcher is a typo that silently scrubs nothing.
        for name in LAUNCHER_NOISE {
            assert!(is_launcher_noise(name), "{name}");
        }
        for prefix in LAUNCHER_NOISE_PREFIXES {
            assert!(is_launcher_noise(&format!("{prefix}SOMETHING")), "{prefix}");
        }
    }

    #[test]
    fn exact_names_match_either_case() {
        // Windows compares environment names case-insensitively; a scrub that only caught the
        // canonical spelling would miss no_color on the platform that had the incident.
        assert!(is_launcher_noise("NO_COLOR"));
        assert!(is_launcher_noise("no_color"));
        assert!(is_launcher_noise("No_Color"));
        assert!(is_launcher_noise("wt_session"));
        assert!(is_launcher_noise("GITHUB_ACTIONS"));
    }

    #[test]
    fn prefix_matching_applies_only_to_listed_prefixes() {
        // CLAUDE_CODE_FOO matches through the prefix rule...
        assert!(is_launcher_noise("CLAUDE_CODE_FOO"));
        assert!(is_launcher_noise("claude_code_mcp_servers"));
        // ...but a prefix entry is not a licence for "starts with the same letters": CI must
        // not swallow CIRCLE, and NO_COLOR must not swallow longer no-colour-ish names.
        assert!(!is_launcher_noise("CIRCLE"));
        assert!(!is_launcher_noise("CITY"));
        assert!(!is_launcher_noise("NO_COLORS_PLEASE"));
        assert!(!is_launcher_noise("WT_SESSIONS"));
    }

    #[test]
    fn a_non_ascii_name_straddling_the_prefix_length_is_not_noise_and_does_not_panic() {
        // Windows allows any Unicode in a variable name. Eleven ASCII bytes followed by a
        // three-byte character put a char boundary past the CLAUDE_CODE_ prefix length (12),
        // which a byte-index slice would panic on — at the top of main, for every command.
        assert!(!is_launcher_noise("ABCDEFGHIJK\u{4e2d}"));
        assert!(!is_launcher_noise("\u{4e2d}"));
        assert!(is_launcher_noise("CLAUDE_CODE_\u{4e2d}"));
    }

    #[test]
    fn the_machines_own_variables_are_not_noise() {
        // The blacklist philosophy: everything that is not listed survives. These are the
        // ones the spec promised not to touch, plus the gateway's own knobs.
        for name in [
            "PATH",
            "HOME",
            "USERPROFILE",
            "SystemRoot",
            "COMSPEC",
            "PATHEXT",
            "TEMP",
            "TMP",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "NO_PROXY",
            "SWISS_HOME",
            "SWISS_PORT",
            "SWISS_TOKEN",
        ] {
            assert!(!is_launcher_noise(name), "{name}");
        }
    }

    #[test]
    fn scrubbing_a_command_strikes_the_noise_and_only_the_noise() {
        use std::process::Command;

        // SAFETY: test-only planting of env vars; this test binary tolerates the race the
        // same way every other env-planting test in the workspace does.
        unsafe {
            std::env::set_var("NO_COLOR", "1");
            std::env::set_var("CLAUDE_CODE_TEST_MARKER", "1");
        }
        let mut command = Command::new("swiss-no-such-binary");
        command.env("PATH_KEY_OVERRIDDEN", "x"); // an explicit env set must survive
        scrub_command(&mut command);
        let removed: Vec<String> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        assert!(removed.iter().any(|n| n.eq_ignore_ascii_case("NO_COLOR")));
        assert!(removed
            .iter()
            .any(|n| n.eq_ignore_ascii_case("CLAUDE_CODE_TEST_MARKER")));
        assert!(removed.iter().any(|n| n.eq_ignore_ascii_case("WT_SESSION")));
        // The explicit set is still a set, not a removal.
        assert!(command
            .get_envs()
            .any(|(name, value)| name == "PATH_KEY_OVERRIDDEN" && value.is_some()));
        // SAFETY: restore the machine for the rest of the test binary.
        unsafe {
            std::env::remove_var("NO_COLOR");
            std::env::remove_var("CLAUDE_CODE_TEST_MARKER");
        }
    }
}
