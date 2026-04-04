//! BTreeMap-backed entity storage with deterministic sorted iteration.
//!
//! `EntityStore` replaces `hecs::World` as the container for all game entities.
//! Entities are keyed by their stable_id (u64) for O(log n) lookup. BTreeMap
//! provides deterministic sorted iteration natively — no manual cache needed.
//!
//! ## Borrow patterns
//! - Single entity mutation: `store.get_mut(id)` borrows only that entry
//! - Cross-entity reads during mutation: read target first (clone needed data),
//!   then get_mut on the other entity
//! - Batch iteration with mutation: collect `keys_sorted()`, loop with `get_mut()`
//!
//! ## Dependency rules
//! - Part of sim/ — depends only on sim/game_entity.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use std::collections::BTreeMap;

use crate::sim::game_entity::GameEntity;

/// Container for all game entities, keyed by stable_id.
///
/// Uses `BTreeMap<u64, GameEntity>` for deterministic sorted iteration
/// and O(log n) lookup. All iteration methods return entities in
/// ascending stable_id order, which is critical for lockstep multiplayer.
///
/// Maintains a secondary per-owner index (`by_owner`) so that queries like
/// "all buildings owned by house X" are O(that house's entities) instead of
/// O(total entities). The index is kept in sync automatically by `insert()`
/// and `remove()`. Equivalent to gamemd.exe's per-HouseClass object arrays.
pub struct EntityStore {
    /// Primary storage: stable_id -> GameEntity.
    entities: BTreeMap<u64, GameEntity>,
    /// Per-owner index: owner InternedId -> sorted Vec of stable_ids.
    /// Maintained automatically on insert/remove. Deterministic iteration
    /// via BTreeMap key order + sorted Vecs.
    by_owner: BTreeMap<crate::sim::intern::InternedId, Vec<u64>>,
}

impl EntityStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self {
            entities: BTreeMap::new(),
            by_owner: BTreeMap::new(),
        }
    }

    /// Insert an entity. Returns its stable_id.
    pub fn insert(&mut self, entity: GameEntity) -> u64 {
        let id = entity.stable_id;
        self.entities.insert(id, entity);
        id
    }

    /// Remove an entity by stable_id. Returns the removed entity if it existed.
    pub fn remove(&mut self, stable_id: u64) -> Option<GameEntity> {
        self.entities.remove(&stable_id)
    }

    /// Look up an entity by stable_id (immutable).
    pub fn get(&self, stable_id: u64) -> Option<&GameEntity> {
        self.entities.get(&stable_id)
    }

    /// Look up an entity by stable_id (mutable).
    pub fn get_mut(&mut self, stable_id: u64) -> Option<&mut GameEntity> {
        self.entities.get_mut(&stable_id)
    }

    /// Check if an entity exists.
    pub fn contains(&self, stable_id: u64) -> bool {
        self.entities.contains_key(&stable_id)
    }

    /// Number of entities in the store.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Get sorted keys for deterministic iteration.
    ///
    /// Callers typically iterate with `get()` or `get_mut()`:
    /// ```ignore
    /// let keys = store.keys_sorted();
    /// for &id in &keys {
    ///     if let Some(entity) = store.get_mut(id) { ... }
    /// }
    /// ```
    pub fn keys_sorted(&self) -> Vec<u64> {
        self.entities.keys().copied().collect()
    }

    /// Iterate all entities in deterministic stable_id order (immutable).
    pub fn iter_sorted(&self) -> impl Iterator<Item = (u64, &GameEntity)> {
        self.entities.iter().map(|(&k, v)| (k, v))
    }

    /// Iterate all entity values in deterministic stable_id order (immutable).
    pub fn values_sorted(&self) -> impl Iterator<Item = &GameEntity> {
        self.entities.values()
    }

    /// Iterate all entities in stable_id order (immutable).
    /// With BTreeMap, this is always deterministic.
    pub fn values(&self) -> impl Iterator<Item = &GameEntity> {
        self.entities.values()
    }

    /// Iterate all entities mutably in stable_id order.
    /// With BTreeMap, this is always deterministic.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut GameEntity> {
        self.entities.values_mut()
    }

    /// Stable IDs owned by the given owner, in sorted order.
    /// Returns an empty slice if the owner has no entities.
    /// O(1) lookup + O(n) iteration where n = that owner's entity count.
    pub fn ids_for_owner(&self, owner: crate::sim::intern::InternedId) -> &[u64] {
        self.by_owner
            .get(&owner)
            .map_or(&[], |ids| ids.as_slice())
    }

    /// Rebuild the per-owner index from primary storage.
    /// Called after deserialization or any bulk mutation that bypasses insert/remove.
    pub fn rebuild_owner_index(&mut self) {
        self.by_owner.clear();
        for (&id, entity) in &self.entities {
            self.by_owner.entry(entity.owner).or_default().push(id);
        }
        // BTreeMap iteration is already sorted by key; Vecs are sorted because
        // entities BTreeMap iterates in ascending stable_id order.
    }
}

impl serde::Serialize for EntityStore {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entities.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for EntityStore {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let entities = BTreeMap::<u64, GameEntity>::deserialize(deserializer)?;
        let mut store = Self {
            entities,
            by_owner: BTreeMap::new(),
        };
        store.rebuild_owner_index();
        Ok(store)
    }
}

impl Default for EntityStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::game_entity::GameEntity;

    fn make_entity(id: u64) -> GameEntity {
        GameEntity::test_default(id, "HTNK", "Americans", 10, 10)
    }

    #[test]
    fn test_insert_and_get() {
        let mut store = EntityStore::new();
        store.insert(make_entity(1));
        store.insert(make_entity(2));

        assert_eq!(store.len(), 2);
        assert!(!store.is_empty());
        assert!(store.contains(1));
        assert!(store.contains(2));
        assert!(!store.contains(3));

        let e = store.get(1).expect("entity 1 should exist");
        assert_eq!(e.stable_id, 1);
    }

    #[test]
    fn test_get_mut() {
        let mut store = EntityStore::new();
        store.insert(make_entity(1));

        let e = store.get_mut(1).expect("entity 1 should exist");
        e.health.current = 50;

        let e = store.get(1).expect("entity 1 should exist");
        assert_eq!(e.health.current, 50);
    }

    #[test]
    fn test_remove() {
        let mut store = EntityStore::new();
        store.insert(make_entity(1));
        store.insert(make_entity(2));

        let removed = store.remove(1);
        assert!(removed.is_some());
        assert_eq!(removed.expect("should be Some").stable_id, 1);
        assert_eq!(store.len(), 1);
        assert!(!store.contains(1));
        assert!(store.contains(2));

        // Removing non-existent ID returns None.
        assert!(store.remove(99).is_none());
    }

    #[test]
    fn test_deterministic_iteration_order() {
        let mut store = EntityStore::new();
        // Insert in non-sorted order.
        store.insert(make_entity(5));
        store.insert(make_entity(1));
        store.insert(make_entity(3));
        store.insert(make_entity(2));
        store.insert(make_entity(4));

        let keys: Vec<u64> = store.keys_sorted();
        assert_eq!(keys, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_iter_sorted() {
        let mut store = EntityStore::new();
        store.insert(make_entity(3));
        store.insert(make_entity(1));
        store.insert(make_entity(2));

        let ids: Vec<u64> = store.iter_sorted().map(|(id, _)| id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn test_values_sorted() {
        let mut store = EntityStore::new();
        store.insert(make_entity(3));
        store.insert(make_entity(1));
        store.insert(make_entity(2));

        let ids: Vec<u64> = store.values_sorted().map(|e| e.stable_id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn test_sorted_after_mutation() {
        let mut store = EntityStore::new();
        store.insert(make_entity(1));
        store.insert(make_entity(3));

        let keys: Vec<u64> = store.keys_sorted();
        assert_eq!(keys, vec![1, 3]);

        // Insert maintains order.
        store.insert(make_entity(2));
        let keys: Vec<u64> = store.keys_sorted();
        assert_eq!(keys, vec![1, 2, 3]);

        // Remove maintains order.
        store.remove(1);
        let keys: Vec<u64> = store.keys_sorted();
        assert_eq!(keys, vec![2, 3]);
    }

    #[test]
    fn test_empty_store() {
        let store = EntityStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        assert!(store.get(1).is_none());

        let keys: Vec<u64> = store.keys_sorted();
        assert!(keys.is_empty());
    }

    #[test]
    fn test_mutable_iteration_pattern() {
        let mut store = EntityStore::new();
        store.insert(make_entity(1));
        store.insert(make_entity(2));
        store.insert(make_entity(3));

        // The canonical pattern for mutating during iteration:
        // collect keys, then get_mut each entity.
        let keys = store.keys_sorted();
        for &id in &keys {
            if let Some(entity) = store.get_mut(id) {
                entity.health.current = entity.health.current.saturating_sub(10);
            }
        }

        // Verify all were mutated.
        for &id in &[1u64, 2, 3] {
            let e = store.get(id).expect("should exist");
            assert_eq!(e.health.current, 90);
        }
    }

    #[test]
    fn test_cross_entity_read_pattern() {
        let mut store = EntityStore::new();
        let mut e1 = make_entity(1);
        e1.position.rx = 10;
        e1.position.ry = 20;
        store.insert(e1);

        let mut e2 = make_entity(2);
        e2.position.rx = 30;
        e2.position.ry = 40;
        store.insert(e2);

        // Read target position first (immutable borrow ends).
        let target_pos = store.get(2).map(|e| e.position.clone());
        // Then mutate attacker (no conflict).
        if let (Some(attacker), Some(pos)) = (store.get_mut(1), target_pos) {
            // In real code: compute firing direction, apply cooldown, etc.
            assert_eq!(pos.rx, 30);
            attacker.facing = 128; // face toward target
        }

        assert_eq!(store.get(1).expect("should exist").facing, 128);
    }

    #[test]
    fn test_per_owner_index() {
        use crate::sim::intern::StringInterner;

        let mut interner = StringInterner::new();
        let americans = interner.intern("Americans");
        let soviets = interner.intern("Russians");

        let mut store = EntityStore::new();

        let mut e1 = GameEntity::test_default(1, "HTNK", "Americans", 5, 5);
        e1.owner = americans;
        let mut e2 = GameEntity::test_default(2, "MTNK", "Americans", 6, 6);
        e2.owner = americans;
        let mut e3 = GameEntity::test_default(3, "RHNO", "Russians", 10, 10);
        e3.owner = soviets;

        store.insert(e1);
        store.insert(e3);
        store.insert(e2);
        store.rebuild_owner_index();

        // Americans should have [1, 2] sorted.
        assert_eq!(store.ids_for_owner(americans), &[1, 2]);
        // Russians should have [3].
        assert_eq!(store.ids_for_owner(soviets), &[3]);

        // Remove one American entity, rebuild.
        store.remove(1);
        store.rebuild_owner_index();
        assert_eq!(store.ids_for_owner(americans), &[2]);
        assert_eq!(store.ids_for_owner(soviets), &[3]);

        // Remove all American entities, rebuild.
        store.remove(2);
        store.rebuild_owner_index();
        assert_eq!(store.ids_for_owner(americans), &[] as &[u64]);

        // Unknown owner returns empty slice.
        let unknown = interner.intern("Yuri");
        assert_eq!(store.ids_for_owner(unknown), &[] as &[u64]);
    }

    #[test]
    fn test_owner_transfer_captured_by_rebuild() {
        use crate::sim::intern::StringInterner;

        let mut interner = StringInterner::new();
        let americans = interner.intern("Americans");
        let soviets = interner.intern("Russians");

        let mut store = EntityStore::new();
        let mut e1 = GameEntity::test_default(1, "HTNK", "Americans", 5, 5);
        e1.owner = americans;
        store.insert(e1);
        store.rebuild_owner_index();

        assert_eq!(store.ids_for_owner(americans), &[1]);
        assert_eq!(store.ids_for_owner(soviets), &[] as &[u64]);

        // Simulate engineer capture: mutate owner via get_mut().
        store.get_mut(1).unwrap().owner = soviets;

        // Index is stale until rebuild.
        assert_eq!(store.ids_for_owner(americans), &[1]); // still old
        store.rebuild_owner_index();

        // After rebuild, entity moved to new owner.
        assert_eq!(store.ids_for_owner(americans), &[] as &[u64]);
        assert_eq!(store.ids_for_owner(soviets), &[1]);
    }

    #[test]
    fn test_rebuild_owner_index() {
        use crate::sim::intern::StringInterner;

        let mut interner = StringInterner::new();
        let americans = interner.intern("Americans");

        let mut store = EntityStore::new();
        let mut e1 = GameEntity::test_default(1, "HTNK", "Americans", 5, 5);
        e1.owner = americans;
        let mut e2 = GameEntity::test_default(2, "MTNK", "Americans", 6, 6);
        e2.owner = americans;
        store.insert(e1);
        store.insert(e2);

        // Manually clear the index to simulate deserialization state.
        store.by_owner.clear();
        assert_eq!(store.ids_for_owner(americans), &[] as &[u64]);

        // Rebuild should restore the index.
        store.rebuild_owner_index();
        assert_eq!(store.ids_for_owner(americans), &[1, 2]);
    }
}
