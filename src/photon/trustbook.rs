//! Remembered trust of gateways, keyed by network address and by payout script, so that a reconnect does not reset it.
//!
//! * a key is `ip:<address>` or `script:<hex>`; a connection uses the keys it knows and takes the least trusted entry
//! * failures are forgiven one per `DECAY_SECS` without a new failure
//! * three failures ban a payout script for `BAN_SECS` (after the ban one more failure bans again); an address is never banned,
//!   because many gateways can sit behind one address, but its failures keep the spot check probability high
use super::verify::Trust;
use std::collections::{HashMap, HashSet};

pub const DECAY_SECS: i64 = 24 * 3600;
pub const BAN_SECS: i64 = 3600;
/// Entries without activity for this long are dropped.
pub const MAX_AGE_SECS: i64 = 30 * 24 * 3600;
pub const MAX_ENTRIES: usize = 200_000;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    pub trust: Trust,
    pub last_fail: i64,
    pub banned_until: i64,
    pub updated: i64,
}

impl Entry {
    fn settle(&mut self, now: i64) {
        while self.trust.fails > 0 && self.last_fail > 0 && now - self.last_fail >= DECAY_SECS {
            self.trust.fails -= 1;
            self.last_fail += DECAY_SECS;
        }
        if self.banned_until != 0 && now >= self.banned_until {
            self.banned_until = 0;
            self.trust.fails = self.trust.fails.min(Trust::BAN_AFTER - 1);
        }
    }
}

#[derive(Default)]
pub struct TrustBook {
    map: HashMap<String, Entry>,
    dirty: HashSet<String>,
}

impl TrustBook {
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn load(&mut self, rows: Vec<(String, Entry)>) {
        for (k, e) in rows {
            self.map.insert(k, e);
        }
    }

    pub fn get(&mut self, key: &str, now: i64) -> Option<Entry> {
        let e = self.map.get_mut(key)?;
        e.settle(now);
        Some(e.clone())
    }

    /// Probability of the next spot check: the highest among the keys that are known; unknown keys count only when none is known.
    pub fn probability(&mut self, keys: &[String], now: i64) -> f64 {
        let known: Vec<f64> = keys.iter().filter_map(|k| self.get(k, now)).map(|e| e.trust.probability()).collect();
        if known.is_empty() {
            Trust::default().probability()
        } else {
            known.into_iter().fold(0.0, f64::max)
        }
    }

    pub fn banned(&mut self, keys: &[String], now: i64) -> bool {
        keys.iter().any(|k| self.get(k, now).map_or(false, |e| e.banned_until > now))
    }

    /// Records a check result under all keys; returns true when a key is now banned.
    pub fn record(&mut self, keys: &[String], passed: bool, now: i64) -> bool {
        let mut banned = false;
        for k in keys {
            let e = self.map.entry(k.clone()).or_default();
            e.settle(now);
            e.trust.record(passed);
            e.updated = now;
            if !passed {
                e.last_fail = now;
                if k.starts_with("script:") && e.trust.banned() && e.banned_until == 0 {
                    e.banned_until = now + BAN_SECS;
                }
            }
            banned |= e.banned_until > now;
            self.dirty.insert(k.clone());
        }
        banned
    }

    pub fn take_dirty(&mut self) -> Vec<(String, Entry)> {
        let keys: Vec<String> = self.dirty.drain().collect();
        keys.into_iter().filter_map(|k| self.map.get(&k).map(|e| (k, e.clone()))).collect()
    }

    /// Drops old entries (and the oldest ones beyond the size limit); returns the removed keys.
    pub fn prune(&mut self, now: i64) -> Vec<String> {
        let mut gone: Vec<String> = self.map.iter().filter(|(_, e)| now - e.updated > MAX_AGE_SECS && e.banned_until <= now).map(|(k, _)| k.clone()).collect();
        if self.map.len() - gone.len().min(self.map.len()) > MAX_ENTRIES {
            let mut rest: Vec<(&String, i64)> = self.map.iter().filter(|(k, _)| !gone.contains(k)).map(|(k, e)| (k, e.updated)).collect();
            rest.sort_by_key(|(_, u)| *u);
            let excess = rest.len() - MAX_ENTRIES;
            gone.extend(rest.into_iter().take(excess).map(|(k, _)| k.clone()));
        }
        for k in &gone {
            self.map.remove(k);
            self.dirty.remove(k);
        }
        gone
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Vec<String> {
        vec!["ip:10.0.0.1".into(), "script:aa".into()]
    }

    #[test]
    fn trust_survives_a_reconnect_and_is_shared_between_keys() {
        let mut b = TrustBook::default();
        assert_eq!(b.probability(&keys(), 0), 0.5);
        for _ in 0..200 {
            b.record(&keys(), true, 0);
        }
        assert!((b.probability(&keys(), 0) - 0.02).abs() < 1e-9);
        // the same payout script from a new address is as trusted as before
        assert!((b.probability(&["ip:10.0.0.9".into(), "script:aa".into()], 0) - 0.02).abs() < 1e-9);
        // a fresh script from the old address too
        assert!((b.probability(&["ip:10.0.0.1".into(), "script:bb".into()], 0) - 0.02).abs() < 1e-9);
        // but a brand new identity starts at the default
        assert_eq!(b.probability(&["ip:10.0.0.9".into(), "script:bb".into()], 0), 0.5);
    }

    #[test]
    fn the_least_trusted_key_decides() {
        let mut b = TrustBook::default();
        for _ in 0..200 {
            b.record(&["ip:a".into()], true, 0);
        }
        b.record(&["script:x".into()], false, 0);
        assert!(b.probability(&["ip:a".into(), "script:x".into()], 0) > 0.3);
    }

    #[test]
    fn three_failures_ban_a_script_for_an_hour_and_one_more_bans_again() {
        let mut b = TrustBook::default();
        assert!(!b.record(&keys(), false, 100));
        assert!(!b.record(&keys(), false, 101));
        assert!(b.record(&keys(), false, 102));
        assert!(b.banned(&keys(), 103));
        assert!(b.banned(&["ip:other".into(), "script:aa".into()], 103), "the script key alone is enough");
        assert!(!b.banned(&["ip:10.0.0.1".into(), "script:other".into()], 103), "the address of a banned script is not banned");
        assert!(!b.banned(&keys(), 102 + BAN_SECS));
        // after the ban a single failure bans again
        assert!(b.record(&keys(), false, 102 + BAN_SECS + 5));
        assert!(b.banned(&keys(), 102 + BAN_SECS + 6));
    }

    #[test]
    fn failures_are_forgiven_one_per_day() {
        let mut b = TrustBook::default();
        for _ in 0..40 {
            b.record(&["ip:a".into()], true, 1000);
        }
        b.record(&["ip:a".into()], false, 1000);
        b.record(&["ip:a".into()], false, 1000);
        let p0 = b.probability(&["ip:a".into()], 1000);
        assert!(p0 > 0.5);
        let p1 = b.probability(&["ip:a".into()], 1000 + DECAY_SECS);
        let p2 = b.probability(&["ip:a".into()], 1000 + 2 * DECAY_SECS);
        assert!(p0 > p1 && p1 > p2, "{p0} {p1} {p2}");
        assert_eq!(b.get("ip:a", 1000 + 2 * DECAY_SECS).unwrap().trust.fails, 0);
    }

    #[test]
    fn dirty_entries_are_handed_out_once_and_old_ones_pruned() {
        let mut b = TrustBook::default();
        b.record(&keys(), true, 5);
        assert_eq!(b.take_dirty().len(), 2);
        assert!(b.take_dirty().is_empty());
        assert!(b.prune(5 + 10).is_empty());
        assert_eq!(b.prune(5 + MAX_AGE_SECS + 1).len(), 2);
        assert_eq!(b.len(), 0);
    }

    #[test]
    fn banned_entries_are_kept_until_the_ban_ends() {
        let mut b = TrustBook::default();
        for _ in 0..3 {
            b.record(&["script:evil".into()], false, 10);
        }
        assert!(b.prune(10 + 100).is_empty());
        assert!(b.banned(&["script:evil".into()], 10 + 100));
    }

    #[test]
    fn an_address_with_failures_is_checked_always_but_never_banned() {
        let mut b = TrustBook::default();
        for _ in 0..5 {
            assert!(!b.record(&["ip:nat".into()], false, 10));
        }
        assert!(!b.banned(&["ip:nat".into()], 11));
        assert_eq!(b.probability(&["ip:nat".into()], 11), 1.0);
        // an honest gateway behind the same address with its own clean script is still limited by the address' probability
        assert_eq!(b.probability(&["ip:nat".into(), "script:clean".into()], 11), 1.0);
    }
}
