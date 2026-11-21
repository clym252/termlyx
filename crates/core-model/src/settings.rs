//! Connection-settings inheritance: app defaults, then the group chain
//! (root to leaf), then the host, where `Some` at a later level overrides.

use std::collections::{HashMap, HashSet};

use crate::entities::{ConnSettings, Group, Host, Id};

/// Port used when neither the app defaults nor any group nor the host sets one.
pub const FALLBACK_PORT: u16 = 22;

/// Which level supplied an effective value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// App defaults (also used when nothing at all set the value).
    Default,
    /// The group with this id.
    Group(Id),
    /// The host itself.
    Host,
}

/// An effective value together with the level it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved<T> {
    pub value: T,
    pub source: Source,
}

/// Fully resolved settings for a host: one [`Resolved`] per [`ConnSettings`]
/// field. `port` always has a value (see [`FALLBACK_PORT`]); every other field
/// stays `Option` because "unset" is a meaningful answer for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSettings {
    pub port: Resolved<u16>,
    pub identity_id: Resolved<Option<Id>>,
    pub keepalive_secs: Resolved<Option<u32>>,
    pub term_type: Resolved<Option<String>>,
    pub charset: Resolved<Option<String>>,
    pub color_scheme: Resolved<Option<String>>,
    pub font_family: Resolved<Option<String>>,
    pub font_size: Resolved<Option<u16>>,
    pub env: Resolved<Option<Vec<(String, String)>>>,
}

/// The group parent links form a loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("group hierarchy contains a cycle at group {group_id}")]
pub struct CycleError {
    /// A group that was reached twice while walking up the parents.
    pub group_id: Id,
}

/// Ancestors of `leaf` (inclusive), ordered root to leaf.
///
/// `None` gives an empty chain. A group id that is not in `groups` (a dangling
/// `parent_id`, or a dangling `leaf`) ends the walk: the last existing group is
/// treated as the root, and the missing id is not part of the chain.
pub fn group_chain(groups: &HashMap<Id, Group>, leaf: Option<Id>) -> Result<Vec<Id>, CycleError> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut next = leaf;
    while let Some(id) = next {
        let Some(group) = groups.get(&id) else {
            break;
        };
        if !seen.insert(id) {
            return Err(CycleError { group_id: id });
        }
        chain.push(id);
        next = group.parent_id;
    }
    chain.reverse();
    Ok(chain)
}

/// Compute the effective settings of `host`:
/// `defaults` < groups (root to leaf) < host.
pub fn resolve(
    defaults: &ConnSettings,
    groups: &HashMap<Id, Group>,
    host: &Host,
) -> Result<EffectiveSettings, CycleError> {
    let chain = group_chain(groups, host.group_id)?;
    // Lowest to highest priority; later layers win.
    let mut layers: Vec<(Source, &ConnSettings)> = vec![(Source::Default, defaults)];
    layers.extend(
        chain
            .iter()
            .map(|id| (Source::Group(*id), &groups[id].settings)),
    );
    layers.push((Source::Host, &host.settings));

    Ok(EffectiveSettings {
        port: {
            let port = pick_layer(&layers, |s| &s.port);
            Resolved {
                value: port.value.unwrap_or(FALLBACK_PORT),
                source: port.source,
            }
        },
        identity_id: pick_layer(&layers, |s| &s.identity_id),
        keepalive_secs: pick_layer(&layers, |s| &s.keepalive_secs),
        term_type: pick_layer(&layers, |s| &s.term_type),
        charset: pick_layer(&layers, |s| &s.charset),
        color_scheme: pick_layer(&layers, |s| &s.color_scheme),
        font_family: pick_layer(&layers, |s| &s.font_family),
        font_size: pick_layer(&layers, |s| &s.font_size),
        env: pick_layer(&layers, |s| &s.env),
    })
}

/// Highest-priority `Some` for one field, or `None` attributed to `Default`.
fn pick_layer<T: Clone>(
    layers: &[(Source, &ConnSettings)],
    field: impl Fn(&ConnSettings) -> &Option<T>,
) -> Resolved<Option<T>> {
    layers
        .iter()
        .rev()
        .find_map(|(source, settings)| {
            field(settings).as_ref().map(|value| Resolved {
                value: Some(value.clone()),
                source: *source,
            })
        })
        .unwrap_or(Resolved {
            value: None,
            source: Source::Default,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn id() -> Id {
        Id::now_v7()
    }

    fn group(id: Id, parent_id: Option<Id>, settings: ConnSettings) -> Group {
        Group {
            id,
            parent_id,
            name: "g".into(),
            settings,
            extra: Default::default(),
        }
    }

    fn host(group_id: Option<Id>, settings: ConnSettings) -> Host {
        Host {
            id: id(),
            group_id,
            label: "h".into(),
            address: "example.org".into(),
            tag_ids: vec![],
            settings,
            forward_rule_ids: vec![],
            extra: Default::default(),
        }
    }

    fn map(groups: Vec<Group>) -> HashMap<Id, Group> {
        groups.into_iter().map(|g| (g.id, g)).collect()
    }

    #[test]
    fn host_overrides_group_overrides_parent() {
        let (parent, child) = (id(), id());
        let groups = map(vec![
            group(
                parent,
                None,
                ConnSettings {
                    port: Some(2200),
                    ..Default::default()
                },
            ),
            group(
                child,
                Some(parent),
                ConnSettings {
                    font_size: Some(14),
                    ..Default::default()
                },
            ),
        ]);
        let h = host(
            Some(child),
            ConnSettings {
                port: Some(22),
                ..Default::default()
            },
        );
        let eff = resolve(&ConnSettings::default(), &groups, &h).unwrap();
        assert_eq!(
            eff.port,
            Resolved {
                value: 22,
                source: Source::Host
            }
        );
        assert_eq!(
            eff.font_size,
            Resolved {
                value: Some(14),
                source: Source::Group(child)
            }
        );
    }

    #[test]
    fn group_overrides_parent_and_parent_fills_gaps() {
        let (parent, child) = (id(), id());
        let groups = map(vec![
            group(
                parent,
                None,
                ConnSettings {
                    port: Some(2200),
                    term_type: Some("vt100".into()),
                    ..Default::default()
                },
            ),
            group(
                child,
                Some(parent),
                ConnSettings {
                    port: Some(2300),
                    ..Default::default()
                },
            ),
        ]);
        let h = host(Some(child), ConnSettings::default());
        let eff = resolve(&ConnSettings::default(), &groups, &h).unwrap();
        assert_eq!(
            eff.port,
            Resolved {
                value: 2300,
                source: Source::Group(child)
            }
        );
        assert_eq!(
            eff.term_type,
            Resolved {
                value: Some("vt100".into()),
                source: Source::Group(parent)
            }
        );
    }

    #[test]
    fn defaults_fill_gaps() {
        let defaults = ConnSettings {
            keepalive_secs: Some(30),
            ..Default::default()
        };
        let h = host(None, ConnSettings::default());
        let eff = resolve(&defaults, &HashMap::new(), &h).unwrap();
        assert_eq!(
            eff.keepalive_secs,
            Resolved {
                value: Some(30),
                source: Source::Default
            }
        );
        // Nothing set anywhere: None / fallback port, both attributed to Default.
        assert_eq!(
            eff.font_size,
            Resolved {
                value: None,
                source: Source::Default
            }
        );
        assert_eq!(
            eff.port,
            Resolved {
                value: FALLBACK_PORT,
                source: Source::Default
            }
        );
    }

    #[test]
    fn cycle_detected() {
        let (a, b) = (id(), id());
        let groups = map(vec![
            group(a, Some(b), ConnSettings::default()),
            group(b, Some(a), ConnSettings::default()),
        ]);
        assert!(matches!(
            group_chain(&groups, Some(a)),
            Err(CycleError { .. })
        ));
        let h = host(Some(a), ConnSettings::default());
        assert!(resolve(&ConnSettings::default(), &groups, &h).is_err());
    }

    #[test]
    fn self_parent_is_a_cycle() {
        let a = id();
        let groups = map(vec![group(a, Some(a), ConnSettings::default())]);
        assert_eq!(
            group_chain(&groups, Some(a)),
            Err(CycleError { group_id: a })
        );
    }

    #[test]
    fn chain_is_root_to_leaf_and_dangling_parent_is_root() {
        let (root, mid, leaf, missing) = (id(), id(), id(), id());
        let groups = map(vec![
            group(root, None, ConnSettings::default()),
            group(mid, Some(root), ConnSettings::default()),
            group(leaf, Some(mid), ConnSettings::default()),
        ]);
        assert_eq!(
            group_chain(&groups, Some(leaf)).unwrap(),
            vec![root, mid, leaf]
        );
        assert_eq!(group_chain(&groups, None).unwrap(), Vec::<Id>::new());

        let orphan = id();
        let groups = map(vec![group(orphan, Some(missing), ConnSettings::default())]);
        assert_eq!(group_chain(&groups, Some(orphan)).unwrap(), vec![orphan]);
        // A dangling leaf yields an empty chain.
        assert_eq!(
            group_chain(&groups, Some(missing)).unwrap(),
            Vec::<Id>::new()
        );
    }
}
