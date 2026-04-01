//! Super-zone connected components via union-find on the zone adjacency graph.
//!
//! After flood-fill partitions the map into zones and adjacency is extracted,
//! this module computes connected components: groups of zones that are
//! transitively reachable through adjacency edges. This turns the O(V+E) BFS
//! in `zone_graph_connected()` into an O(1) lookup.
//!
//! ## Dependency rules
//! - Part of sim/ — depends only on sim/zone_map types.
//! - sim/ NEVER depends on render/, ui/, sidebar/, audio/, net/.

use super::zone_map::{ZONE_INVALID, ZoneAdjacency, ZoneId};

/// Connected-component labels for zones, computed via union-find.
/// Two zones with the same label are transitively reachable.
#[derive(Debug, Clone)]
pub(crate) struct SuperZoneMap {
    /// For each zone ID (1-indexed), the component label.
    /// Index 0 is unused (ZONE_INVALID). Labels are canonical root zone IDs.
    labels: Vec<ZoneId>,
}

impl SuperZoneMap {
    /// Build super-zone labels from zone adjacency using union-find.
    pub fn from_adjacency(adj: &ZoneAdjacency, zone_count: u16) -> Self {
        let n = zone_count as usize + 1; // 1-indexed zones
        let mut parent: Vec<usize> = (0..n).collect();
        let mut rank: Vec<u8> = vec![0; n];

        // Union all adjacent zone pairs.
        for z in 1..=zone_count as usize {
            for &neighbor in adj.neighbors_of(z as ZoneId) {
                union(&mut parent, &mut rank, z, neighbor as usize);
            }
        }

        // Path-compress to get final labels.
        let labels: Vec<ZoneId> = (0..n).map(|i| find(&mut parent, i) as ZoneId).collect();

        SuperZoneMap { labels }
    }

    /// O(1) reachability: are two zones in the same connected component?
    pub fn are_connected(&self, a: ZoneId, b: ZoneId) -> bool {
        if a == ZONE_INVALID || b == ZONE_INVALID {
            return false;
        }
        let ai = a as usize;
        let bi = b as usize;
        if ai >= self.labels.len() || bi >= self.labels.len() {
            return false;
        }
        self.labels[ai] == self.labels[bi]
    }
}

// ---------------------------------------------------------------------------
// Union-find with path compression and union by rank
// ---------------------------------------------------------------------------

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]]; // path halving
        x = parent[x];
    }
    x
}

fn union(parent: &mut [usize], rank: &mut [u8], a: usize, b: usize) {
    let ra = find(parent, a);
    let rb = find(parent, b);
    if ra == rb {
        return;
    }
    // Union by rank: attach smaller tree under larger.
    if rank[ra] < rank[rb] {
        parent[ra] = rb;
    } else if rank[ra] > rank[rb] {
        parent[rb] = ra;
    } else {
        parent[rb] = ra;
        rank[ra] += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adj_from_edges(zone_count: u16, edges: &[(ZoneId, ZoneId)]) -> ZoneAdjacency {
        let mut neighbors: Vec<Vec<ZoneId>> = vec![Vec::new(); zone_count as usize + 1];
        for &(a, b) in edges {
            neighbors[a as usize].push(b);
            neighbors[b as usize].push(a);
        }
        for list in &mut neighbors {
            list.sort_unstable();
            list.dedup();
        }
        ZoneAdjacency::new(neighbors)
    }

    #[test]
    fn single_zone_connected_to_self() {
        let adj = adj_from_edges(1, &[]);
        let sz = SuperZoneMap::from_adjacency(&adj, 1);
        assert!(sz.are_connected(1, 1));
    }

    #[test]
    fn two_adjacent_zones_connected() {
        let adj = adj_from_edges(2, &[(1, 2)]);
        let sz = SuperZoneMap::from_adjacency(&adj, 2);
        assert!(sz.are_connected(1, 2));
    }

    #[test]
    fn two_isolated_zones_not_connected() {
        let adj = adj_from_edges(2, &[]);
        let sz = SuperZoneMap::from_adjacency(&adj, 2);
        assert!(!sz.are_connected(1, 2));
    }

    #[test]
    fn transitive_connectivity() {
        // A-B, B-C → A and C are connected via B.
        let adj = adj_from_edges(3, &[(1, 2), (2, 3)]);
        let sz = SuperZoneMap::from_adjacency(&adj, 3);
        assert!(sz.are_connected(1, 3));
        assert!(sz.are_connected(1, 2));
        assert!(sz.are_connected(2, 3));
    }

    #[test]
    fn two_components() {
        // Zones 1-2 connected, zone 3 isolated.
        let adj = adj_from_edges(3, &[(1, 2)]);
        let sz = SuperZoneMap::from_adjacency(&adj, 3);
        assert!(sz.are_connected(1, 2));
        assert!(!sz.are_connected(1, 3));
        assert!(!sz.are_connected(2, 3));
    }

    #[test]
    fn invalid_zone_never_connected() {
        let adj = adj_from_edges(2, &[(1, 2)]);
        let sz = SuperZoneMap::from_adjacency(&adj, 2);
        assert!(!sz.are_connected(ZONE_INVALID, 1));
        assert!(!sz.are_connected(1, ZONE_INVALID));
    }
}
