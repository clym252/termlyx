//! `~/.ssh/config` import.
//!
//! [`plan_ssh_config_import`] turns config text into an [`ImportPlan`]: pure
//! apart from reading `$HOME` in the convenience wrapper (the `_with_home`
//! variant takes the home directory and touches no file). It never writes to
//! the vault; `VaultService::apply_import` does that, in one transaction.
//!
//! Mapping:
//! * Every concrete `Host` alias becomes one [`Host`] (`label` = alias,
//!   `address` = `HostName` with `%h` replaced, else the alias, `port` =
//!   `Port` if set). A `Host a b` line yields two hosts.
//! * Values are resolved the way `ssh` does, through `ssh2-config`'s
//!   `query(alias)`: the first value found wins, across the host's own block,
//!   matching wildcard blocks (`Host *.corp`), `Host *` and options before
//!   the first `Host`. `IdentityFile` accumulates; the first file is used
//!   (an identity holds one key).
//! * `User` and `IdentityFile` become an [`Identity`]. Label: `user
//!   (keyfile-name)`, or `user` without a key, or `keyfile-name` without a
//!   user (then `username` is empty). Identities are shared within one
//!   import by (user, key path).
//! * A host with neither `User` nor `IdentityFile` gets no identity. Both
//!   cases connect the way `ssh` does (ruling R31, in the app's
//!   `connect_params`): an empty or missing username is the local login
//!   name, and a host without an identity uses keyboard-interactive, which
//!   falls back to a password prompt.
//! * Skipped, with a reason, never silently dropped: patterns containing `*`,
//!   `?` or `!`; aliases that equal an existing host's label; hosts with
//!   `ProxyJump`/`ProxyCommand` (until Phase 4); `Match` blocks; `Include`
//!   lines (not followed, to keep planning free of file access); and a
//!   config that does not parse at all.
//!
//! `ssh2-config` has no notion of `Match` (it would attach a `Match` block's
//! options to the previous `Host`), follows `Include` on the real
//! filesystem, and expands `~` with the real home directory. A small
//! line-level pre-pass ([`prefilter`]) removes `Match` blocks and `Include`
//! lines and expands `~` (also `%d`, `${HOME}`, `%USERPROFILE%`...) in
//! `IdentityFile` against the injected home before the text reaches the crate.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use core_model::{ConnSettings, Host, Id, Identity};
use serde::Serialize;
use ssh2_config::{HostParams, ParseRule, SshConfig};

/// What an ssh_config import would create. Nothing is written until the plan
/// is handed to `VaultService::apply_import`.
#[derive(Debug, Clone, Default)]
pub struct ImportPlan {
    pub hosts: Vec<Host>,
    /// Identities referenced by `hosts`. `key_id` is `None` here;
    /// `apply_import` fills it in once the key file has loaded.
    pub identities: Vec<Identity>,
    /// The unique key files to load, in order of first use.
    pub key_files: Vec<PathBuf>,
    /// Which key file belongs to which identity (identities without a key do
    /// not appear).
    pub identity_key_files: Vec<(Id, PathBuf)>,
    /// `(alias or directive, reason)` for everything not imported.
    pub skipped: Vec<(String, String)>,
}

/// The outcome of `VaultService::apply_import`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    pub hosts_added: usize,
    pub identities_added: usize,
    pub keys_added: usize,
    /// Keys that could not be loaded. Their identities were still stored,
    /// without a key.
    pub key_failures: Vec<KeyFailure>,
}

/// A key file that could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyFailure {
    pub path: PathBuf,
    /// The key is encrypted: the UI can ask for the passphrase and retry.
    pub needs_passphrase: bool,
    /// The loader's error, never containing key material.
    pub message: String,
}

/// Plans the import of `text` (an ssh_config) against the vault's `existing`
/// hosts, expanding `~` against the current user's home directory.
pub fn plan_ssh_config_import(text: &str, existing: &[Host]) -> ImportPlan {
    plan_ssh_config_import_with_home(text, existing, std::env::home_dir().as_deref())
}

/// [`plan_ssh_config_import`] with an explicit home directory. With `None`,
/// `~` in `IdentityFile` is left to the key loader to reject.
pub fn plan_ssh_config_import_with_home(
    text: &str,
    existing: &[Host],
    home: Option<&Path>,
) -> ImportPlan {
    let mut plan = ImportPlan::default();
    let (filtered, pre_skipped) = prefilter(text, home);
    plan.skipped = pre_skipped;

    let rules = ParseRule::ALLOW_UNKNOWN_FIELDS | ParseRule::ALLOW_UNSUPPORTED_FIELDS;
    let config = match SshConfig::default().parse(&mut filtered.as_bytes(), rules) {
        Ok(config) => config,
        Err(err) => {
            plan.skipped
                .push(("ssh_config".to_owned(), format!("could not parse: {err}")));
            return plan;
        }
    };

    // Entry 0 holds the options before the first `Host` line (global
    // defaults); `query` already folds it in.
    let mut seen = HashSet::new();
    let mut identities: HashMap<(String, Option<PathBuf>), Id> = HashMap::new();
    for clause in config.get_hosts().iter().skip(1).flat_map(|h| &h.pattern) {
        let alias = clause.to_string();
        if !seen.insert(alias.clone()) {
            continue;
        }
        if clause.negated || clause.pattern.contains(['*', '?', '!']) {
            plan.skipped
                .push((alias, "wildcard pattern (not a concrete host)".to_owned()));
            continue;
        }
        if existing.iter().any(|h| h.label == alias) {
            plan.skipped.push((alias, "already exists".to_owned()));
            continue;
        }
        let params = config.query(&alias);
        if let Some(reason) = proxy_reason(&params) {
            plan.skipped.push((alias, reason));
            continue;
        }

        let key_file = params
            .identity_file
            .as_ref()
            .and_then(|files| files.first())
            .cloned();
        let identity_id = (params.user.is_some() || key_file.is_some()).then(|| {
            let username = params.user.clone().unwrap_or_default();
            *identities
                .entry((username.clone(), key_file.clone()))
                .or_insert_with(|| {
                    let identity = Identity {
                        id: Id::now_v7(),
                        label: identity_label(&username, key_file.as_deref()),
                        username,
                        password: None,
                        key_id: None,
                        owner: None,
                        extra: Default::default(),
                    };
                    let id = identity.id;
                    plan.identities.push(identity);
                    if let Some(file) = &key_file {
                        plan.identity_key_files.push((id, file.clone()));
                        if !plan.key_files.contains(file) {
                            plan.key_files.push(file.clone());
                        }
                    }
                    id
                })
        });

        plan.hosts.push(Host {
            id: Id::now_v7(),
            group_id: None,
            address: params
                .host_name
                .as_deref()
                .map_or_else(|| alias.clone(), |name| name.replace("%h", &alias)),
            label: alias,
            tag_ids: vec![],
            settings: ConnSettings {
                port: params.port,
                identity_id,
                ..ConnSettings::default()
            },
            forward_rule_ids: vec![],
            extra: Default::default(),
        });
    }
    plan
}

/// Why `params` needs a proxy this phase cannot provide, if it does.
/// `none` switches a proxy off, as in ssh.
fn proxy_reason(params: &HostParams) -> Option<String> {
    let is_none = |args: &[String]| args.first().is_some_and(|a| a.eq_ignore_ascii_case("none"));
    if params
        .proxy_jump
        .as_deref()
        .is_some_and(|jumps| !jumps.is_empty() && !is_none(jumps))
    {
        return Some("ProxyJump not supported until Phase 4".to_owned());
    }
    if params
        .unsupported_fields
        .get("proxycommand")
        .is_some_and(|args| !is_none(args))
    {
        return Some("ProxyCommand not supported until Phase 4 (as with ProxyJump)".to_owned());
    }
    None
}

fn identity_label(username: &str, key_file: Option<&Path>) -> String {
    let key_name = key_file
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned());
    match (username.is_empty(), key_name) {
        (false, Some(key)) => format!("{username} ({key})"),
        (false, None) => username.to_owned(),
        (true, Some(key)) => key,
        (true, None) => String::new(),
    }
}

/// Splits a config line into its keyword and arguments (`Key value` or
/// `Key=value`).
fn split_keyword(line: &str) -> (&str, &str) {
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let rest = line[end..].trim_start();
    (&line[..end], rest.strip_prefix('=').unwrap_or(rest).trim())
}

/// Removes what `ssh2-config` cannot handle faithfully (see the module docs)
/// and returns the text to parse plus a skipped entry for each removal.
fn prefilter(text: &str, home: Option<&Path>) -> (String, Vec<(String, String)>) {
    let mut out = String::with_capacity(text.len());
    let mut skipped = Vec::new();
    let mut in_match = false;
    for line in text.lines() {
        let trimmed = line.trim();
        let (keyword, rest) = split_keyword(trimmed);
        let keyword = keyword.to_ascii_lowercase();
        let args = rest.split('#').next().unwrap_or_default().trim();
        match keyword.as_str() {
            _ if trimmed.starts_with('#') || trimmed.is_empty() => {}
            "match" => {
                in_match = true;
                skipped.push((
                    format!("Match {args}"),
                    "Match blocks are not supported".to_owned(),
                ));
                continue;
            }
            "host" => in_match = false,
            _ if in_match => continue,
            "include" => {
                skipped.push((
                    format!("Include {args}"),
                    "Include is not followed; import that file separately".to_owned(),
                ));
                continue;
            }
            "identityfile" => {
                if let Some(expanded) = home.and_then(|home| expand_home(args, home)) {
                    out.push_str(&format!("IdentityFile \"{expanded}\"\n"));
                    continue;
                }
            }
            _ => {}
        }
        out.push_str(line);
        out.push('\n');
    }
    (out, skipped)
}

/// Environment-variable spellings of the user's home directory that Windows
/// configs use for `IdentityFile`. Variable names are case-insensitive there,
/// so these match regardless of case. (ssh's own `%d` token is separate and
/// case-sensitive.)
const HOME_VARS: [&str; 4] = [
    "${HOME}",
    "${USERPROFILE}",
    "%USERPROFILE%",
    "%HOMEDRIVE%%HOMEPATH%",
];

/// `value` without a leading `prefix`, ignoring ASCII case.
fn strip_prefix_ignore_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let head = value.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &value[prefix.len()..])
}

/// A leading home spelling (`~`, `~/...`, `~\...`, `%d`, or one of
/// [`HOME_VARS`]) in the first argument of an `IdentityFile` line, expanded
/// against `home` and escaped for a quoted ssh_config argument.
/// What follows the home keeps its own separators (Windows takes either).
fn expand_home(args: &str, home: &Path) -> Option<String> {
    let value = match args.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next()?,
        None => args.split_whitespace().next()?,
    };
    let rest = match value.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => rest,
        Some(_) => return None, // `~user/...` is not supported
        None => match value.strip_prefix("%d") {
            Some(rest) => rest,
            None => HOME_VARS
                .iter()
                .find_map(|var| strip_prefix_ignore_case(value, var))?,
        },
    };
    let path = format!("{}{rest}", home.display());
    Some(path.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use core_model::{ConnSettings, SshKey};
    use core_ssh::keys::{GeneratedKey, KeyError};
    use secrecy::{ExposeSecret, SecretString};

    use super::*;
    use crate::vault::{MemoryKeyStore, VaultError, VaultService};

    const SAMPLE: &str = include_str!("../../tests/data/ssh_config_sample");
    const HOME: &str = "/home/tester";

    fn plan(text: &str, existing: &[Host]) -> ImportPlan {
        plan_ssh_config_import_with_home(text, existing, Some(Path::new(HOME)))
    }

    fn existing_host(label: &str) -> Host {
        Host {
            id: Id::now_v7(),
            group_id: None,
            label: label.into(),
            address: "old.example.com".into(),
            tag_ids: vec![],
            settings: ConnSettings::default(),
            forward_rule_ids: vec![],
            extra: Default::default(),
        }
    }

    fn host<'a>(plan: &'a ImportPlan, label: &str) -> &'a Host {
        plan.hosts
            .iter()
            .find(|h| h.label == label)
            .unwrap_or_else(|| panic!("no host {label}: {:?}", plan.skipped))
    }

    fn identity_of<'a>(plan: &'a ImportPlan, host: &Host) -> &'a Identity {
        let id = host.settings.identity_id.expect("host has an identity");
        plan.identities.iter().find(|i| i.id == id).unwrap()
    }

    fn skipped_reason<'a>(plan: &'a ImportPlan, alias: &str) -> &'a str {
        plan.skipped
            .iter()
            .find(|(a, _)| a == alias)
            .unwrap_or_else(|| panic!("{alias} not skipped: {:?}", plan.skipped))
            .1
            .as_str()
    }

    #[test]
    fn maps_host_hostname_port_user_identityfile() {
        let p = plan(SAMPLE, &[existing_host("already-here")]);
        let web = host(&p, "web");
        assert_eq!(web.address, "web.example.com");
        assert_eq!(web.settings.port, Some(2222));
        let identity = identity_of(&p, web);
        assert_eq!(identity.username, "deploy");
        assert_eq!(identity.label, "deploy (id_deploy)");
        assert!(identity.password.is_none());
        assert!(identity.key_id.is_none(), "keys are linked by apply_import");
        let key = PathBuf::from("/home/tester/.ssh/id_deploy");
        assert_eq!(p.key_files, vec![key.clone()]);
        assert!(p.identity_key_files.contains(&(identity.id, key)));
        // No Port line: the app default applies later.
        assert_eq!(host(&p, "db1").settings.port, None);
    }

    #[test]
    fn multi_alias_line_yields_one_host_per_alias() {
        let p = plan(SAMPLE, &[]);
        for alias in ["db1", "db2"] {
            assert_eq!(host(&p, alias).address, "db.example.com");
        }
    }

    #[test]
    fn identities_deduplicated_by_user_and_key() {
        let p = plan(SAMPLE, &[]);
        let web = identity_of(&p, host(&p, "web")).id;
        assert_eq!(identity_of(&p, host(&p, "db1")).id, web);
        assert_eq!(identity_of(&p, host(&p, "db2")).id, web);
        // deploy (shared) and corpuser; ops went away with its skipped host.
        let mut users: Vec<&str> = p.identities.iter().map(|i| i.username.as_str()).collect();
        users.sort();
        assert_eq!(users, ["corpuser", "deploy"]);
    }

    #[test]
    fn wildcard_patterns_skipped_with_reason() {
        let p = plan(SAMPLE, &[]);
        for pattern in ["*", "*.corp"] {
            assert!(
                skipped_reason(&p, pattern).contains("wildcard"),
                "{pattern}: {:?}",
                p.skipped
            );
            assert!(p.hosts.iter().all(|h| h.label != pattern));
        }
        let p = plan("Host web? !jump app\n  HostName a.example.com\n", &[]);
        assert!(skipped_reason(&p, "web?").contains("wildcard"));
        assert!(skipped_reason(&p, "!jump").contains("wildcard"));
        assert_eq!(p.hosts.len(), 1);
        assert_eq!(p.hosts[0].label, "app");
    }

    #[test]
    fn wildcard_blocks_supply_defaults_like_ssh() {
        let p = plan(SAMPLE, &[]);
        // intranet.corp matches "Host *.corp", which sets the user.
        let intranet = host(&p, "intranet.corp");
        assert_eq!(intranet.address, "10.1.1.1");
        let identity = identity_of(&p, intranet);
        assert_eq!(identity.username, "corpuser");
        assert_eq!(identity.label, "corpuser");

        // A global default applies to hosts that do not set the value; the
        // host's own value wins.
        let text = "Host a\n  Port 2200\nHost b\nHost *\n  Port 22\n  User ops\n";
        let p = plan(text, &[]);
        assert_eq!(host(&p, "a").settings.port, Some(2200));
        assert_eq!(host(&p, "b").settings.port, Some(22));
        assert_eq!(identity_of(&p, host(&p, "a")).username, "ops");
        assert_eq!(p.identities.len(), 1, "same user, shared identity");
    }

    #[test]
    fn duplicates_by_alias_not_reimported() {
        let existing = existing_host("already-here");
        let p = plan(SAMPLE, std::slice::from_ref(&existing));
        assert_eq!(skipped_reason(&p, "already-here"), "already exists");
        assert!(p.hosts.iter().all(|h| h.label != "already-here"));
        // Without the existing host it is imported.
        assert_eq!(
            host(&plan(SAMPLE, &[]), "already-here").address,
            "already.example.com"
        );
    }

    #[test]
    fn proxyjump_noted_as_skipped_until_phase4() {
        let p = plan(SAMPLE, &[]);
        let reason = skipped_reason(&p, "behind-bastion");
        assert!(
            reason.contains("ProxyJump") && reason.contains("Phase 4"),
            "{reason}"
        );
        assert!(p.hosts.iter().all(|h| h.label != "behind-bastion"));
        // Its user is not imported either.
        assert!(p.identities.iter().all(|i| i.username != "ops"));

        let p = plan("Host cmd\n  ProxyCommand ssh -W %h:%p gw\n", &[]);
        let reason = skipped_reason(&p, "cmd");
        assert!(
            reason.contains("ProxyCommand") && reason.contains("Phase 4"),
            "{reason}"
        );

        // "none" switches the proxy off.
        let p = plan(
            "Host a\n  ProxyJump none\nHost b\n  ProxyCommand none\n",
            &[],
        );
        assert_eq!(p.hosts.len(), 2, "{:?}", p.skipped);
    }

    #[test]
    fn host_without_hostname_uses_alias_and_percent_h() {
        let p = plan("Host plain\nHost tpl\n  HostName %h.example.com\n", &[]);
        assert_eq!(host(&p, "plain").address, "plain");
        assert_eq!(host(&p, "plain").settings.identity_id, None);
        assert_eq!(host(&p, "tpl").address, "tpl.example.com");
        assert!(p.identities.is_empty());
    }

    #[test]
    fn key_without_user_and_user_without_key() {
        let text = "Host k\n  IdentityFile /keys/id_k\nHost u\n  User bob\n";
        let p = plan(text, &[]);
        let k = identity_of(&p, host(&p, "k"));
        assert_eq!((k.username.as_str(), k.label.as_str()), ("", "id_k"));
        let u = identity_of(&p, host(&p, "u"));
        assert_eq!(u.username, "bob");
        assert_eq!(p.key_files, vec![PathBuf::from("/keys/id_k")]);
        assert!(p.identity_key_files.iter().all(|(id, _)| *id != u.id));
    }

    #[test]
    fn first_identity_file_wins_and_tilde_uses_injected_home() {
        let text =
            "Host a\n  User u\n  IdentityFile ~/.ssh/one\n  IdentityFile \"~/my keys/two\"\n";
        let p = plan(text, &[]);
        assert_eq!(p.key_files, vec![PathBuf::from("/home/tester/.ssh/one")]);
        let text = "Host a\n  User u\n  IdentityFile \"~/my keys/two\"\n";
        let p = plan(text, &[]);
        assert_eq!(p.key_files, vec![PathBuf::from("/home/tester/my keys/two")]);
        // Unknown home: the path is left for the key loader to reject.
        let p = plan_ssh_config_import_with_home(text, &[], None);
        assert_eq!(p.key_files.len(), 1);
        assert!(!p.key_files[0].starts_with("/home/tester"));
    }

    #[test]
    fn expands_windows_home_forms_with_spaces_and_unicode() {
        let home = Path::new(r"C:\Users\Zoë Lee");
        // The home is escaped for a quoted argument; the rest keeps the
        // separators it was written with.
        let backslashes = r"C:\\Users\\Zoë Lee\\.ssh\\id";
        let mixed = r"C:\\Users\\Zoë Lee/.ssh/id";
        for (input, want) in [
            (r"~\.ssh\id", backslashes),
            ("~/.ssh/id", mixed),
            ("%d/.ssh/id", mixed),
            ("${HOME}/.ssh/id", mixed),
            (r"${USERPROFILE}\.ssh\id", backslashes),
            (r"%USERPROFILE%\.ssh\id", backslashes),
            (r"%HOMEDRIVE%%HOMEPATH%\.ssh\id", backslashes),
            (r#""~\.ssh\id" trailing"#, backslashes),
            // Environment variable names are case-insensitive on Windows.
            (r"%UserProfile%\.ssh\id", backslashes),
            (r"${userprofile}\.ssh\id", backslashes),
            (r"${Home}/.ssh/id", mixed),
            (r"%HomeDrive%%HomePath%\.ssh\id", backslashes),
        ] {
            assert_eq!(expand_home(input, home).as_deref(), Some(want), "{input}");
        }
        assert_eq!(
            expand_home("~", home).as_deref(),
            Some(r"C:\\Users\\Zoë Lee")
        );
        // `%d` is ssh's own token: lower case only.
        assert_eq!(expand_home("%D/.ssh/id", home), None);
        assert_eq!(expand_home("~bob/x", home), None);
        assert_eq!(expand_home(r"~bob\x", home), None);
        assert_eq!(expand_home("/keys/id", home), None);
        assert_eq!(expand_home(r"C:\keys\id", home), None);
        // Through the whole pre-pass, the file lands where Windows OpenSSH looks.
        let text = "Host a\n  User u\n  IdentityFile ~\\.ssh\\id\n";
        let p = plan_ssh_config_import_with_home(text, &[], Some(home));
        assert_eq!(
            p.key_files,
            vec![PathBuf::from(r"C:\Users\Zoë Lee\.ssh\id")]
        );
    }

    #[test]
    fn match_blocks_skipped_and_do_not_leak_into_previous_host() {
        let text = "Host a\n  HostName a.example.com\nMatch host b exec true\n  Port 9999\n  User leaked\nHost c\n  HostName c.example.com\n";
        let p = plan(text, &[]);
        let (alias, reason) = p
            .skipped
            .iter()
            .find(|(a, _)| a.starts_with("Match"))
            .unwrap();
        assert!(alias.contains("host b"), "{alias}");
        assert!(reason.contains("Match"), "{reason}");
        assert_eq!(host(&p, "a").settings.port, None);
        assert!(p.identities.is_empty());
        assert_eq!(host(&p, "c").address, "c.example.com");
    }

    #[test]
    fn include_is_not_followed() {
        let p = plan("Include ~/.ssh/config.d/*\nHost a\n", &[]);
        assert!(skipped_reason(&p, "Include ~/.ssh/config.d/*").contains("not followed"));
        assert_eq!(p.hosts.len(), 1);
    }

    #[test]
    fn unparsable_config_is_reported_not_panicked() {
        let p = plan("Host a\n  Port notaport\n", &[]);
        assert!(p.hosts.is_empty());
        assert!(skipped_reason(&p, "ssh_config").contains("could not parse"));
    }

    #[test]
    fn unknown_directives_and_comments_are_tolerated() {
        let text = "# c\nUseRoaming no\nHost a # inline\n  FancyNewOption 1\n  HostName a.example.com # x\n";
        let p = plan(text, &[]);
        assert_eq!(host(&p, "a").address, "a.example.com");
    }

    // -- apply_import

    fn vault() -> (tempfile::TempDir, VaultService) {
        let dir = tempfile::tempdir().unwrap();
        let mut v = VaultService::open_for_tests(
            &dir.path().join("vault.sqlite"),
            Box::new(MemoryKeyStore::default()),
        )
        .unwrap();
        v.create(SecretString::from("correct horse battery staple"))
            .unwrap();
        (dir, v)
    }

    fn fake_key(file: &Path) -> GeneratedKey {
        let name = file.file_name().unwrap().to_string_lossy();
        GeneratedKey {
            private_openssh: SecretString::from(format!("PRIVATE {name}")),
            public_openssh: format!("ssh-ed25519 AAAA{name}"),
            sha256_fingerprint: "SHA256:fake".into(),
            algorithm: "ssh-ed25519".into(),
        }
    }

    const TWO_KEYS: &str = "Host a\n  User u\n  IdentityFile /keys/id_a\n\
                            Host b\n  User u\n  IdentityFile /keys/id_b\n\
                            Host c\n  User u\n  IdentityFile /keys/id_a\n";

    #[test]
    fn apply_import_stores_keys_identities_and_hosts() {
        let (_dir, mut v) = vault();
        let report = v
            .apply_import(plan(SAMPLE, &[]), |p| Ok(fake_key(p)))
            .unwrap();
        assert_eq!(
            report,
            ImportReport {
                hosts_added: 5,
                identities_added: 2,
                keys_added: 1,
                key_failures: vec![],
            }
        );

        let keys = v.list::<SshKey>().unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].label, "id_deploy");
        assert_eq!(keys[0].algorithm, "ssh-ed25519");
        assert_eq!(keys[0].private_openssh.expose_secret(), "PRIVATE id_deploy");
        assert!(keys[0].passphrase.is_none());

        let hosts = v.list::<Host>().unwrap();
        let web = hosts.iter().find(|h| h.label == "web").unwrap();
        let identity = v
            .get::<Identity>(web.settings.identity_id.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(identity.username, "deploy");
        assert_eq!(identity.key_id, Some(keys[0].id));
        // The shared identity serves all three hosts.
        let sharing = hosts
            .iter()
            .filter(|h| h.settings.identity_id == Some(identity.id))
            .count();
        assert_eq!(sharing, 3);
        // And the corp identity has no key.
        let corp = hosts.iter().find(|h| h.label == "intranet.corp").unwrap();
        let corp = v
            .get::<Identity>(corp.settings.identity_id.unwrap())
            .unwrap()
            .unwrap();
        assert!(corp.key_id.is_none());
    }

    #[test]
    fn apply_import_survives_a_bad_key_and_reports_it() {
        let (_dir, mut v) = vault();
        let report = v
            .apply_import(plan(TWO_KEYS, &[]), |p| {
                if p.ends_with("id_a") {
                    Err(KeyError::NeedsPassphrase)
                } else {
                    Ok(fake_key(p))
                }
            })
            .unwrap();
        assert_eq!(report.hosts_added, 3);
        assert_eq!(report.identities_added, 2);
        assert_eq!(report.keys_added, 1);
        assert_eq!(
            report.key_failures,
            vec![KeyFailure {
                path: PathBuf::from("/keys/id_a"),
                needs_passphrase: true,
                message: KeyError::NeedsPassphrase.to_string(),
            }]
        );

        // Both identities are stored; only the one whose key loaded has it.
        let keys = v.list::<SshKey>().unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].label, "id_b");
        let identities = v.list::<Identity>().unwrap();
        assert_eq!(identities.len(), 2);
        assert_eq!(
            identities
                .iter()
                .filter_map(|i| i.key_id)
                .collect::<Vec<_>>(),
            vec![keys[0].id]
        );
        assert_eq!(v.list::<Host>().unwrap().len(), 3);

        // A key that is simply broken is reported without the flag.
        let (_dir, mut v) = vault();
        let report = v
            .apply_import(plan(TWO_KEYS, &[]), |_| Err(KeyError::Parse("junk".into())))
            .unwrap();
        assert_eq!(report.keys_added, 0);
        assert_eq!(report.key_failures.len(), 2);
        assert!(report.key_failures.iter().all(|f| !f.needs_passphrase));
        assert_eq!(v.list::<Identity>().unwrap().len(), 2);
    }

    #[test]
    fn apply_import_writes_nothing_when_a_record_is_rejected() {
        let (_dir, mut v) = vault();
        let mut p = plan(TWO_KEYS, &[]);
        // The last host points at an identity that is not in the plan.
        p.hosts.last_mut().unwrap().settings.identity_id = Some(Id::now_v7());
        let err = v.apply_import(p, |p| Ok(fake_key(p))).unwrap_err();
        assert!(matches!(err, VaultError::MissingRef(_)), "{err}");
        assert!(v.list::<SshKey>().unwrap().is_empty());
        assert!(v.list::<Identity>().unwrap().is_empty());
        assert!(v.list::<Host>().unwrap().is_empty());
    }

    #[test]
    fn apply_import_requires_an_unlocked_vault() {
        let (_dir, mut v) = vault();
        v.lock();
        let err = v.apply_import(plan(SAMPLE, &[]), |p| Ok(fake_key(p)));
        assert!(matches!(err, Err(VaultError::Locked)));
    }

    #[test]
    fn importing_the_same_file_again_adds_no_duplicate_hosts() {
        let (_dir, mut v) = vault();
        v.apply_import(plan(SAMPLE, &[]), |p| Ok(fake_key(p)))
            .unwrap();
        let existing = v.list::<Host>().unwrap();
        let again = plan(SAMPLE, &existing);
        assert!(again.hosts.is_empty(), "{:?}", again.hosts);
        let report = v.apply_import(again, |p| Ok(fake_key(p))).unwrap();
        assert_eq!((report.hosts_added, report.keys_added), (0, 0));
        assert_eq!(v.list::<Host>().unwrap().len(), 5);
    }
}
