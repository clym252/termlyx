//! Hybrid logical clock: a totally ordered, per-node-unique record version.

use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A hybrid logical clock timestamp.
///
/// Ordering is lexicographic over `(wall_ms, counter, node)`, which is the
/// field order below. The [`Display`](fmt::Display) form is fixed-width
/// lowercase hex (`{wall_ms:016x}-{counter:08x}-{node:04x}`), so comparing the
/// text of two timestamps gives the same result as comparing the values. That
/// lets a store keep an `Hlc` in a plain TEXT column and `ORDER BY` it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hlc {
    /// Milliseconds since the Unix epoch (the highest wall time observed).
    pub wall_ms: u64,
    /// Tie-breaker for events within the same millisecond (or after a clock step back).
    pub counter: u32,
    /// Id of the device that produced the timestamp.
    pub node: u16,
}

impl Hlc {
    /// Next timestamp for `node`, strictly greater than `last`, using the system clock.
    pub fn now(last: &Hlc, node: u16) -> Hlc {
        let wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        Hlc::now_at(last, node, wall_ms)
    }

    /// Next timestamp for `node` after seeing the remote timestamp `seen`,
    /// strictly greater than both `last` and `seen`, using the system clock.
    pub fn observe_now(last: &Hlc, seen: &Hlc, node: u16) -> Hlc {
        let wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        Hlc::observe(last, seen, node, wall_ms)
    }

    /// Pure form of [`Hlc::observe_now`] with the wall clock supplied by the
    /// caller. Takes the greater of `last` and `seen` (by wall time, then
    /// counter) and applies the [`Hlc::now_at`] rule to it, so a device that
    /// pulls a record from a peer whose clock runs ahead never issues a
    /// timestamp that sorts before it.
    pub fn observe(last: &Hlc, seen: &Hlc, node: u16, wall_ms: u64) -> Hlc {
        let newest = if (seen.wall_ms, seen.counter) > (last.wall_ms, last.counter) {
            seen
        } else {
            last
        };
        Hlc::now_at(newest, node, wall_ms)
    }

    /// Pure form of [`Hlc::now`] with the wall clock supplied by the caller.
    ///
    /// If `wall_ms` is ahead of `last` the counter resets to 0. Otherwise (same
    /// millisecond, or the clock went backwards) `last.wall_ms` is kept and the
    /// counter is bumped; on counter overflow the wall time is advanced by 1 ms
    /// instead, so the result is always strictly greater than `last`.
    pub fn now_at(last: &Hlc, node: u16, wall_ms: u64) -> Hlc {
        if wall_ms > last.wall_ms {
            return Hlc {
                wall_ms,
                counter: 0,
                node,
            };
        }
        match last.counter.checked_add(1) {
            Some(counter) => Hlc {
                wall_ms: last.wall_ms,
                counter,
                node,
            },
            None => Hlc {
                wall_ms: last.wall_ms.saturating_add(1),
                counter: 0,
                node,
            },
        }
    }
}

impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:016x}-{:08x}-{:04x}",
            self.wall_ms, self.counter, self.node
        )
    }
}

/// Serialized as the fixed-width text form, so it stays sortable in JSON too.
impl Serialize for Hlc {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Hlc {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// The text is not a valid [`Hlc`] (`{wall_ms:016x}-{counter:08x}-{node:04x}`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid hlc text: {0:?}")]
pub struct HlcParseError(pub String);

impl FromStr for Hlc {
    type Err = HlcParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || HlcParseError(s.to_owned());
        let mut parts = s.split('-');
        let (Some(w), Some(c), Some(n), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(err());
        };
        // `from_str_radix` would also accept a leading '+', so check digits first.
        let hex = |part: &str, width: usize| -> Result<u64, HlcParseError> {
            if part.len() == width && part.bytes().all(|b| b.is_ascii_hexdigit()) {
                u64::from_str_radix(part, 16).map_err(|_| err())
            } else {
                Err(err())
            }
        };
        Ok(Hlc {
            wall_ms: hex(w, 16)?,
            counter: u32::try_from(hex(c, 8)?).map_err(|_| err())?,
            node: u16::try_from(hex(n, 4)?).map_err(|_| err())?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hlc_monotonic() {
        let mut last = Hlc {
            wall_ms: 0,
            counter: 0,
            node: 1,
        };
        for _ in 0..1000 {
            let next = Hlc::now_at(&last, 1, 5_000);
            assert!(next > last, "{next} not > {last}");
            last = next;
        }
        assert_eq!(last.wall_ms, 5_000);
        assert_eq!(last.counter, 999);
    }

    #[test]
    fn wall_clock_advance_resets_counter_and_regression_is_ignored() {
        let last = Hlc {
            wall_ms: 100,
            counter: 7,
            node: 1,
        };
        assert_eq!(
            Hlc::now_at(&last, 1, 200),
            Hlc {
                wall_ms: 200,
                counter: 0,
                node: 1
            }
        );
        // Clock went backwards: keep last.wall_ms, bump the counter.
        assert_eq!(
            Hlc::now_at(&last, 2, 50),
            Hlc {
                wall_ms: 100,
                counter: 8,
                node: 2
            }
        );
    }

    #[test]
    fn counter_overflow_bumps_wall() {
        let last = Hlc {
            wall_ms: 100,
            counter: u32::MAX,
            node: 1,
        };
        let next = Hlc::now_at(&last, 1, 100);
        assert!(next > last);
        assert_eq!(
            next,
            Hlc {
                wall_ms: 101,
                counter: 0,
                node: 1
            }
        );
    }

    #[test]
    fn now_uses_system_clock() {
        let last = Hlc {
            wall_ms: 0,
            counter: 0,
            node: 3,
        };
        let next = Hlc::now(&last, 3);
        assert!(next.wall_ms > 1_700_000_000_000);
        assert!(next > last);
    }

    #[test]
    fn text_form_sorts_like_ord_and_round_trips() {
        let h = |wall_ms, counter, node| Hlc {
            wall_ms,
            counter,
            node,
        };
        let samples = [
            h(0, 0, 0),
            h(0, 0, 1),
            h(0, 1, 0),
            h(9, 0, 0),
            h(10, 0, 0),
            h(255, 15, 0xabc),
            h(256, 0, 0),
            h(u64::MAX, u32::MAX, u16::MAX),
        ];
        for a in &samples {
            let text = a.to_string();
            assert_eq!(text.len(), 16 + 1 + 8 + 1 + 4);
            assert_eq!(text.parse::<Hlc>().unwrap(), *a);
            for b in &samples {
                assert_eq!(a.cmp(b), a.to_string().cmp(&b.to_string()), "{a} vs {b}");
            }
        }
        assert_eq!(
            h(0x1234, 5, 0xab).to_string(),
            "0000000000001234-00000005-00ab"
        );
    }

    #[test]
    fn rejects_malformed_text() {
        for bad in [
            "",
            "1-2-3",
            "zz00000000001234-00000005-00ab",
            "0000000000001234-00000005",
            "0000000000001234-00000005-00ab-1",
            "+000000000001234-00000005-00ab",
        ] {
            assert!(bad.parse::<Hlc>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn observe_jumps_past_remote() {
        let last = Hlc {
            wall_ms: 100,
            counter: 0,
            node: 1,
        };
        let seen = Hlc {
            wall_ms: 500,
            counter: 3,
            node: 2,
        };
        let next = Hlc::observe(&last, &seen, 1, 200);
        assert_eq!(
            next,
            Hlc {
                wall_ms: 500,
                counter: 4,
                node: 1
            }
        );
        assert!(next > last && next > seen);
    }

    #[test]
    fn observe_uses_wall_when_ahead() {
        let last = Hlc {
            wall_ms: 100,
            counter: 0,
            node: 1,
        };
        let seen = Hlc {
            wall_ms: 500,
            counter: 3,
            node: 2,
        };
        assert_eq!(
            Hlc::observe(&last, &seen, 1, 900),
            Hlc {
                wall_ms: 900,
                counter: 0,
                node: 1
            }
        );
    }

    #[test]
    fn observe_takes_local_when_it_is_the_max() {
        let last = Hlc {
            wall_ms: 600,
            counter: 9,
            node: 1,
        };
        let seen = Hlc {
            wall_ms: 500,
            counter: 3,
            node: 2,
        };
        let next = Hlc::observe(&last, &seen, 1, 200);
        assert_eq!(
            next,
            Hlc {
                wall_ms: 600,
                counter: 10,
                node: 1
            }
        );
    }

    #[test]
    fn observe_now_is_above_both_and_uses_system_clock() {
        let last = Hlc {
            wall_ms: 1,
            counter: 0,
            node: 1,
        };
        let far = Hlc {
            wall_ms: u64::MAX / 2,
            counter: 0,
            node: 2,
        };
        assert!(Hlc::observe_now(&last, &far, 1) > far);
        let near = Hlc {
            wall_ms: 2,
            counter: 0,
            node: 2,
        };
        assert!(Hlc::observe_now(&last, &near, 1).wall_ms > 1_700_000_000_000);
    }

    #[test]
    fn serde_uses_the_text_form() {
        let h = Hlc {
            wall_ms: 0x1234,
            counter: 5,
            node: 0xab,
        };
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(json, "\"0000000000001234-00000005-00ab\"");
        assert_eq!(serde_json::from_str::<Hlc>(&json).unwrap(), h);
        assert!(serde_json::from_str::<Hlc>("\"nope\"").is_err());
    }
}
