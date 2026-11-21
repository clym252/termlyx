//! Command builders (and the parser of one's output) for keeping a tab on
//! its tmux session across a reconnect. Nothing here talks to a server.
//!
//! Both commands are sent as an SSH `exec` request, which the server runs
//! through the user's login shell (`$SHELL -c <command>`). That shell may be
//! bash, zsh, dash or fish, so each command is a single `exec /bin/sh -c
//! '<fixed script>'`: the login shell only has to parse that one quoted
//! word, and the script itself is always POSIX `sh`. The fixed scripts have
//! no single quote and no doubled backslash, the two things a single-quoted
//! word treats specially in some shell (fish reads `\\` and `\'` there).

use std::collections::HashMap;

/// Per-channel environment variable that carries a tab's id into its shell
/// (and so into the environment of the tmux client started from it), so the
/// client can be mapped back to the tab. The `LC_` prefix is what stock
/// `AcceptEnv LANG LC_*` lets through.
pub const TAB_ENV: &str = "LC_TETHER_TAB";

/// Startup command for a reconnected tab: attach to tmux session `session`
/// (an exact-name match) if it exists, then, however tmux ended (detach,
/// session killed, tmux gone, or the session vanished before the attach),
/// run the user's login shell. A host without tmux, or whose session is
/// gone, gives the plain login shell and shows no tmux error.
///
/// The name reaches the shell only as lowercase hex of its UTF-8 bytes, a
/// word of `[0-9a-f]` that no shell reads as anything but a word. The script
/// decodes it to the name in `$n` and passes `"=$n"` (exact match) to tmux
/// as one argument, so it is never parsed as code by any shell.
pub fn tmux_reattach_command(session: &str) -> String {
    let hex: String = session.bytes().map(|b| format!("{b:02x}")).collect();
    format!("exec /bin/sh -c '{REATTACH_SCRIPT}' tether-tmux {hex}")
}

/// `$1` is the hex name. Each byte pair becomes an octal escape (`printf
/// "\134"` yields the backslash, so the script holds none itself); `printf`
/// turns the escapes into the name, and a trailing `x` keeps `$(...)` from
/// dropping a name's trailing newlines. `"${SHELL:-/bin/sh}" -l` is the
/// fallback and the end of every path, so the login shell keeps its own
/// start-up files, as a plain `shell` request would. `has-session` picks
/// the path; the attach's own stderr is dropped for the case where the
/// session vanishes in between.
const REATTACH_SCRIPT: &str = r#"h=$1; n=; bs=$(printf "\134"); while [ -n "$h" ]; do r=${h#??}; b=${h%"$r"}; n="$n$bs$(printf %03o "0x$b")"; h=$r; done; n=$(printf "${n}x"); n=${n%x}; tmux has-session -t "=$n" 2>/dev/null && tmux attach -t "=$n" 2>/dev/null; exec "${SHELL:-/bin/sh}" -l"#;

/// Lists which tmux session each tab is in, as `<tab id>` TAB `<session
/// name>` lines (see [`parse_tmux_tabs`]): for every tmux client, the
/// [`TAB_ENV`] value in the client's environment (`/proc/<pid>/environ`,
/// so Linux only). A client without one (started outside Tether) is left
/// out. Prints nothing and exits 0 when tmux is missing, no server runs, or
/// the host has no `/proc`.
///
/// No data reaches the command: it is a fixed string. tmux turns a tab in
/// `-F` output into `_`, so the pid and the name are split at the first
/// space (a pid has none) and the line is read whole (`IFS=`) to keep a
/// name's own leading and trailing spaces.
pub fn tmux_tabs_command() -> &'static str {
    "exec /bin/sh -c 'tmux list-clients -F \"#{client_pid} #{session_name}\" 2>/dev/null | while IFS= read -r l; do p=${l%% *}; s=${l#* }; e=$(tr \"\\0\" \"\\n\" 2>/dev/null <\"/proc/$p/environ\" | sed -n \"s/^LC_TETHER_TAB=//p\" | head -n 1); [ -n \"$e\" ] && printf \"%s\\t%s\\n\" \"$e\" \"$s\"; done; exit 0'"
}

/// Tab id → tmux session name from [`tmux_tabs_command`]'s output. A line
/// that is not UTF-8, has no tab, or has an empty id or name is ignored;
/// the line is split at its first tab. The first line for a tab wins.
pub fn parse_tmux_tabs(stdout: &[u8]) -> HashMap<String, String> {
    let mut tabs = HashMap::new();
    for line in stdout.split(|&b| b == b'\n') {
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        if let Some((tab, session)) = line.split_once('\t')
            && !tab.is_empty()
            && !session.is_empty()
        {
            tabs.entry(tab.to_owned())
                .or_insert_with(|| session.to_owned());
        }
    }
    tabs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names that break naive quoting in some shell.
    pub(crate) const NAMES: &[&str] = &[
        "work",
        "my \"work\" $x",
        "it's",
        "a b\\c `d` $(e) ; f",
        "-x",
        "a\\",
        "\\'",
        "a\\\\b",
        "\\' ; touch /tmp/tether-pwned ; '",
        "h\u{e9}llo \u{2713}",
        "two\nlines",
        "trailing newline\n",
    ];

    #[test]
    fn name_is_a_word_of_lowercase_hex() {
        assert!(tmux_reattach_command("work").ends_with(" tether-tmux 776f726b"));
        for name in NAMES {
            let cmd = tmux_reattach_command(name);
            let word = cmd.rsplit(' ').next().unwrap();
            assert!(
                word.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{name:?}: {cmd}"
            );
            assert_eq!(word.len(), name.len() * 2, "{name:?}");
        }
    }

    #[test]
    fn scripts_have_no_single_quote_or_escaped_backslash() {
        // Inside a single-quoted word fish reads only `\\` and `\'`
        // specially; other backslashes stay as written.
        for script in [REATTACH_SCRIPT, tmux_tabs_command_script()] {
            assert!(!script.contains('\''), "{script}");
            assert!(!script.contains("\\\\"), "{script}");
        }
    }

    #[test]
    fn falls_back_to_login_shell_text_present() {
        let cmd = tmux_reattach_command("a");
        assert!(cmd.starts_with("exec /bin/sh -c '"), "{cmd}");
        assert!(cmd.contains("tmux has-session -t \"=$n\""), "{cmd}");
        // Attach is not exec'd, and the login shell is the last word.
        assert!(!cmd.contains("exec tmux"), "{cmd}");
        assert!(
            cmd.ends_with("exec \"${SHELL:-/bin/sh}\" -l' tether-tmux 61"),
            "{cmd}"
        );
    }

    #[test]
    fn parses_two_tabs() {
        let map = parse_tmux_tabs(b"t1\ta\nt2\tmy \"work\" $x\n");
        assert_eq!(map.len(), 2);
        assert_eq!(map["t1"], "a");
        assert_eq!(map["t2"], "my \"work\" $x");
    }

    #[test]
    fn blank_output_is_no_tabs() {
        assert!(parse_tmux_tabs(b"").is_empty());
        assert!(parse_tmux_tabs(b"\n\n").is_empty());
    }

    #[test]
    fn malformed_lines_are_ignored() {
        let map =
            parse_tmux_tabs(b"garbage\n\tno-tab\nno-session\t\n\xff\xfe\tx\nt3\tb\tc\nt4\ts\n");
        assert_eq!(map.len(), 2, "{map:?}");
        // Only the first tab splits: the rest is the session's name.
        assert_eq!(map["t3"], "b\tc");
        assert_eq!(map["t4"], "s");
    }

    #[test]
    fn tabs_command_reads_the_tab_variable() {
        assert!(tmux_tabs_command().contains(&format!("s/^{TAB_ENV}=//p")));
    }

    #[test]
    fn tabs_command_is_one_sh_word() {
        let cmd = tmux_tabs_command();
        assert!(cmd.starts_with("exec /bin/sh -c '"), "{cmd}");
        assert!(cmd.contains("list-clients"), "{cmd}");
        assert!(cmd.ends_with("exit 0'"), "{cmd}");
    }

    fn tmux_tabs_command_script() -> &'static str {
        let cmd = tmux_tabs_command();
        &cmd["exec /bin/sh -c '".len()..cmd.len() - 1]
    }

    /// Runs the commands through real shells, with a stand-in `tmux` first on
    /// `PATH`.
    #[cfg(unix)]
    mod shells {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        use std::path::{Path, PathBuf};
        use std::process::Command;

        fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        /// Login shells present on this machine, as `$SHELL -c <command>`
        /// would run them.
        fn login_shells() -> Vec<&'static str> {
            [
                "/bin/sh",
                "/bin/bash",
                "/bin/zsh",
                "/bin/dash",
                "/usr/bin/fish",
                "/opt/homebrew/bin/fish",
            ]
            .into_iter()
            .filter(|p| Path::new(p).exists())
            .collect()
        }

        /// The commands name `/bin/sh`; where `dash` exists, also run the
        /// script under it (on macOS `/bin/sh` is bash).
        fn variants(command: &str) -> Vec<String> {
            let mut v = vec![command.to_owned()];
            if Path::new("/bin/dash").exists() {
                v.push(command.replace("exec /bin/sh -c", "exec /bin/dash -c"));
            }
            v
        }

        /// stdout and stderr of `$shell -c command` with `path` as `PATH`.
        fn run(shell: &str, path: &str, command: &str, login_shell: &Path) -> (String, String) {
            let out = Command::new(shell)
                .args(["-c", command])
                .env("PATH", path)
                .env("SHELL", login_shell)
                .output()
                .unwrap();
            (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        }

        fn with_tmux(dir: &Path) -> String {
            format!(
                "{}:{}",
                dir.display(),
                std::env::var("PATH").unwrap_or_default()
            )
        }

        fn temp(tag: &str) -> PathBuf {
            let dir =
                std::env::temp_dir().join(format!("tether-tmux-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        #[test]
        fn attach_targets_the_exact_name_under_every_login_shell() {
            let dir = temp("attach");
            // `tmux has-session ...` succeeds; `tmux attach -t =NAME` prints
            // =NAME. The login shell then prints its marker.
            script(
                &dir,
                "tmux",
                "case $1 in has-session) exit 0;; attach) printf %s \"$3\";; esac",
            );
            let login = script(&dir, "login", "printf '|login %s' \"$*\"");
            let path = with_tmux(&dir);
            for name in NAMES {
                for shell in login_shells() {
                    for cmd in variants(&tmux_reattach_command(name)) {
                        let (got, err) = run(shell, &path, &cmd, &login);
                        assert_eq!(got, format!("={name}|login -l"), "shell {shell}, {name:?}");
                        assert_eq!(err, "", "shell {shell}, {name:?}");
                    }
                }
            }
            assert!(!Path::new("/tmp/tether-pwned").exists());
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn missing_session_runs_the_login_shell_quietly() {
            let dir = temp("gone");
            let login = script(&dir, "login", "printf 'login %s' \"$*\"");
            let path = with_tmux(&dir);
            // tmux present, session gone.
            script(&dir, "tmux", "echo 'no such session' >&2; exit 1");
            for shell in login_shells() {
                for cmd in variants(&tmux_reattach_command("gone")) {
                    assert_eq!(
                        run(shell, &path, &cmd, &login),
                        ("login -l".into(), String::new()),
                        "shell {shell}"
                    );
                }
            }
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn session_vanishing_before_the_attach_runs_the_login_shell_quietly() {
            let dir = temp("race");
            let login = script(&dir, "login", "printf 'login %s' \"$*\"");
            let path = with_tmux(&dir);
            // has-session says yes, attach then fails.
            script(
                &dir,
                "tmux",
                "case $1 in has-session) exit 0;; attach) echo \"can't find session\" >&2; exit 1;; esac",
            );
            for shell in login_shells() {
                for cmd in variants(&tmux_reattach_command("gone")) {
                    assert_eq!(
                        run(shell, &path, &cmd, &login),
                        ("login -l".into(), String::new()),
                        "shell {shell}"
                    );
                }
            }
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn missing_tmux_runs_the_login_shell_quietly() {
            let dir = temp("missing");
            let login = script(&dir, "login", "printf 'login %s' \"$*\"");
            // `PATH` is only the (tmux-less) temp dir, so this holds on a
            // machine that has tmux too.
            let path = dir.display().to_string();
            for shell in login_shells() {
                for cmd in variants(&tmux_reattach_command("gone")) {
                    assert_eq!(
                        run(shell, &path, &cmd, &login),
                        ("login -l".into(), String::new()),
                        "shell {shell}"
                    );
                }
            }
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn tabs_command_maps_clients_to_tabs_and_is_quiet_otherwise() {
            let dir = temp("tabs");
            let nothing = script(&dir, "login", "true");
            let path = with_tmux(&dir);
            // One "client": the shell running the command (the stand-in's
            // parent), whose environment carries the tab id. The name keeps
            // its own spaces.
            script(
                &dir,
                "tmux",
                "[ \"$1\" = list-clients ] || exit 9; echo \"$PPID  s one \"",
            );
            // Only Linux has `/proc/<pid>/environ`; elsewhere, nothing.
            let want = if Path::new("/proc/self/environ").exists() {
                "tab 1\t s one \n"
            } else {
                ""
            };
            for shell in login_shells() {
                for cmd in variants(tmux_tabs_command()) {
                    let out = Command::new(shell)
                        .args(["-c", &cmd])
                        .env("PATH", &path)
                        .env("SHELL", &nothing)
                        .env(TAB_ENV, "tab 1")
                        .output()
                        .unwrap();
                    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "shell {shell}");
                    assert_eq!(String::from_utf8_lossy(&out.stderr), "", "shell {shell}");
                    assert!(out.status.success(), "shell {shell}");
                }
            }
            // A client started outside Tether (no tab id) is left out.
            for shell in login_shells() {
                let (got, err) = run(shell, &path, tmux_tabs_command(), &nothing);
                assert_eq!((got.as_str(), err.as_str()), ("", ""), "shell {shell}");
            }
            // A failing tmux (no server) prints nothing.
            script(&dir, "tmux", "echo 'no server running' >&2; exit 1");
            for shell in login_shells() {
                let (got, err) = run(shell, &path, tmux_tabs_command(), &nothing);
                assert_eq!((got.as_str(), err.as_str()), ("", ""), "shell {shell}");
            }
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
