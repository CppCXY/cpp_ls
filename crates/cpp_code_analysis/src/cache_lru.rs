//! **A cache that forgets**: the entries a session keeps, and the point past which the oldest goes.
//!
//! The session caches things that are expensive to build and cheap to rebuild — a rendering of a file's whole closure,
//! the macro environment its includes define, an effect of its directives — and every one of them was a
//! `HashMap` keyed by `(path, content hash)`. That key is the problem: a keystroke changes the hash, so **typing
//! into one file adds an entry per keystroke** and nothing ever removes one. The observations that prompted this
//! module:
//!
//! ```text
//! · a session left open on a project reported 916 MB before it was touched, and grew while typing
//! · a rendering is the *whole closure's* preprocessed text plus one span per token — for a file that includes
//!   <vector> that is megabytes, not kilobytes, so a handful of stale entries is a gigabyte
//! ```
//!
//! # What it does, and what it deliberately does not
//!
//! Each cache has a cap on **entries** and on **weight** (a byte count for a rendering, a name count for an
//! environment, one for an effect). On insert, the least recently used entries are dropped until both caps hold.
//! Nothing else changes: a `get` that misses is the same miss it was before, so a query answers from the cache when it
//! can and rebuilds when it cannot — which is the arrangement every caller already had.
//!
//! The caps are per cache and chosen from what the value is, not from a global budget: see [`Cache::renderings`] and
//! friends, where each number says what it is counting.
//!
//! # Why not an existing LRU crate
//!
//! The map is small (tens of entries, one effect table), the eviction policy is "smallest stamp first" and the whole
//! point is that the *weight* is a caller's estimate; a dependency for that would be a dependency for twenty lines.

use std::collections::HashMap;
use std::hash::Hash;

/// How many renderings a session keeps — see [`Cache::renderings`].
pub const RENDERING_ENTRIES: usize = 8;

/// How many bytes of rendered text a session keeps — see [`Cache::renderings`].
///
/// One rendering of a header-heavy closure is several megabytes, so this is "a few of them", not "all of them": the
/// cache exists for the file a query is about *right now*, and a file that has scrolled out of use is a rebuild away.
pub const RENDERING_BYTES: usize = 64 * 1024 * 1024;

/// How many macro environments a session keeps — see [`Cache::macro_environments`].
pub const MACRO_ENVIRONMENT_ENTRIES: usize = 16;

/// How many macro *names* across those environments a session keeps — see [`Cache::macro_environments`].
pub const MACRO_ENVIRONMENT_NAMES: usize = 64 * 1024;

/// How many effects a session keeps — see [`Cache::effects`].
pub const EFFECT_ENTRIES: usize = 4096;

/// A least-recently-used map with a weight budget.
///
/// `K` is the cache key (a path and a content hash, or an effect's own hash), `V` the value. The weight is supplied by
/// the caller at insert time because only the caller knows what is large: a rendering's text, an environment's names.
pub struct Cache<K, V> {
    entries: HashMap<K, (V, u64, usize)>,
    clock: u64,
    weight: usize,
    max_entries: usize,
    max_weight: usize,
}

impl<K: Hash + Eq + Clone, V> Cache<K, V> {
    /// A cache of at most `max_entries` entries and `max_weight` of weight.
    pub fn new(max_entries: usize, max_weight: usize) -> Self {
        Self {
            entries: HashMap::new(),
            clock: 0,
            weight: 0,
            max_entries,
            max_weight,
        }
    }

    /// The renderings a session remembers: a few of them, and at most [`RENDERING_BYTES`] of text.
    pub fn renderings() -> Self {
        Self::new(RENDERING_ENTRIES, RENDERING_BYTES)
    }

    /// The macro environments a session remembers: [`MACRO_ENVIRONMENT_NAMES`] names across
    /// [`MACRO_ENVIRONMENT_ENTRIES`] files.
    pub fn macro_environments() -> Self {
        Self::new(MACRO_ENVIRONMENT_ENTRIES, MACRO_ENVIRONMENT_NAMES)
    }

    /// The effects a session remembers. Each is small (a list of folds), so this one is bounded by count alone.
    pub fn effects() -> Self {
        Self::new(EFFECT_ENTRIES, usize::MAX)
    }

    /// The value for `key`, marked as used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.clock += 1;
        let clock = self.clock;
        let (value, used, _) = self.entries.get_mut(key)?;
        *used = clock;
        Some(value)
    }

    /// Whether `key` is held, without marking it used.
    pub fn contains(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    /// Remember `value` under `key`, whose weight is `weight` units of [`Cache::weight`].
    ///
    /// Evicts the least recently used entries until both caps hold. An entry whose own weight exceeds the budget is
    /// still stored (a single rendering larger than the budget is better held than rebuilt on every query) and it
    /// evicts everything else — see [`Cache::insert`]'s ordering.
    pub fn insert(&mut self, key: K, value: V, weight: usize) {
        self.clock += 1;
        let clock = self.clock;
        if let Some((_, _, previous)) = self.entries.remove(&key) {
            self.weight = self.weight.saturating_sub(previous);
        }
        self.entries.insert(key, (value, clock, weight));
        self.weight = self.weight.saturating_add(weight);
        self.evict();
    }

    /// Drop entries until the caps hold, least recently used first.
    fn evict(&mut self) {
        while self.entries.len() > self.max_entries || self.weight > self.max_weight {
            if self.entries.len() <= 1 {
                // One entry that is over the byte budget is still the entry a query just asked for: keep it.
                break;
            }
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used, _))| *used)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else {
                break;
            };
            if let Some((_, _, weight)) = self.entries.remove(&oldest) {
                self.weight = self.weight.saturating_sub(weight);
            }
        }
    }

    /// How many entries are held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is held.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The total weight held.
    pub fn weight(&self) -> usize {
        self.weight
    }

    /// Every value, for a diagnostic that wants to look at them all.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.values().map(|(value, _, _)| value)
    }

    /// Every key, for a diagnostic that names them.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.keys()
    }

    /// Forget `key`.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let (value, _, weight) = self.entries.remove(key)?;
        self.weight = self.weight.saturating_sub(weight);
        Some(value)
    }

    /// Forget every entry **whose key holds a path** the predicate rejects — what a deleted file needs.
    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        let dropped: Vec<K> = self
            .entries
            .iter()
            .filter(|(key, _)| !keep(key))
            .map(|(key, _)| key.clone())
            .collect();
        for key in dropped {
            self.remove(&key);
        }
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.weight = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_evicted_when_the_count_is_exceeded() {
        let mut cache: Cache<u32, &str> = Cache::new(2, usize::MAX);
        cache.insert(1, "one", 1);
        cache.insert(2, "two", 1);
        cache.insert(3, "three", 1);
        assert_eq!(cache.len(), 2, "the cache holds two");
        assert!(!cache.contains(&1), "the oldest went");
        assert!(cache.contains(&2) && cache.contains(&3));
    }

    #[test]
    fn a_used_entry_survives_the_eviction() {
        let mut cache: Cache<u32, &str> = Cache::new(2, usize::MAX);
        cache.insert(1, "one", 1);
        cache.insert(2, "two", 1);
        assert_eq!(cache.get(&1), Some(&"one"), "this marks 1 as used");
        cache.insert(3, "three", 1);
        assert!(cache.contains(&1), "the entry a query just read stays");
        assert!(!cache.contains(&2), "the untouched one goes");
    }

    #[test]
    fn the_weight_budget_evicts_before_the_count_does() {
        let mut cache: Cache<u32, &str> = Cache::new(100, 10);
        cache.insert(1, "one", 6);
        cache.insert(2, "two", 6);
        assert_eq!(cache.len(), 1, "12 units do not fit in 10");
        assert!(cache.contains(&2), "the newer entry is the one kept");
    }

    #[test]
    fn one_entry_over_the_budget_is_kept() {
        let mut cache: Cache<u32, &str> = Cache::new(100, 4);
        cache.insert(1, "huge", 1_000);
        assert_eq!(cache.len(), 1, "a single oversized entry stays");
    }

    #[test]
    fn retain_drops_what_the_predicate_rejects() {
        let mut cache: Cache<u32, &str> = Cache::new(100, usize::MAX);
        cache.insert(1, "one", 1);
        cache.insert(2, "two", 1);
        cache.retain(|key| *key != 1);
        assert_eq!(cache.len(), 1);
        assert!(cache.contains(&2));
        assert_eq!(cache.weight(), 1, "the weight follows the entries");
    }
}
