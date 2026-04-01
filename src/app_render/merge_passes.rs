//! Y-sorted multi-way merge passes for interleaving draw calls across atlas textures.
//!
//! The original engine renders all ground objects in a single Y-sorted pass (Layer 2).
//! Our engine has multiple atlas textures (VXL units, SHP pages 0-3, wall overlays),
//! so we interleave draw calls by walking cursors through each Y-sorted buffer and
//! emitting sub-range draws in depth-descending order (back-to-front).
//!
//! ## Dependency rules
//! - Internal to app_render — only called from draw_passes.rs.

use crate::render::batch::{BatchRenderer, BatchTexture, InstanceBufferPool, SpriteInstance};
use crate::render::overlay_atlas::OverlayAtlas;
use crate::render::sprite_atlas::SpriteAtlas;
use crate::render::unit_atlas::UnitAtlas;

/// Tracks a single draw group during the multi-way merge.
///
/// Each group represents one GPU buffer + texture pair (e.g., VXL units, one SHP page,
/// wall overlays). The `cursor` advances through the buffer as sub-ranges are drawn.
struct DrawGroup<'tex, 'inst> {
    texture: &'tex BatchTexture,
    buffer: &'tex wgpu::Buffer,
    instances: &'inst [SpriteInstance],
    cursor: u32,
    total: u32,
}

impl<'tex, 'inst> DrawGroup<'tex, 'inst> {
    fn new(
        texture: &'tex BatchTexture,
        buffer: &'tex wgpu::Buffer,
        instances: &'inst [SpriteInstance],
        total: u32,
    ) -> Self {
        Self {
            texture,
            buffer,
            instances,
            cursor: 0,
            total,
        }
    }

    fn depth_at(&self, index: u32) -> f32 {
        self.instances
            .get(index as usize)
            .map(|instance| instance.depth)
            .unwrap_or(f32::NEG_INFINITY)
    }
}

/// Multi-way merge for bridge entities: interleaves VXL units and SHP sprites on bridges.
///
/// Draws by depth descending (furthest back first). Only processes bridge-specific
/// pool keys (`unit_bridge`, `shp_bridge_p0..p3`).
pub(super) fn draw_merged_bridge_occluded_pass<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    batch: &'a BatchRenderer,
    pool: &'a InstanceBufferPool,
    unit_instances: &[SpriteInstance],
    shp_paged: &[Vec<SpriteInstance>],
    unit_atlas: Option<&'a UnitAtlas>,
    sprite_atlas: Option<&'a SpriteAtlas>,
) {
    let mut groups: Vec<DrawGroup<'a, '_>> = Vec::new();
    if let (Some(ua), Some((buf, count))) = (unit_atlas, pool.get("unit_bridge")) {
        if count > 0 {
            groups.push(DrawGroup::new(&ua.texture, buf, unit_instances, count));
        }
    }

    const SHP_BRIDGE_KEYS: [&str; 4] = [
        "shp_bridge_p0",
        "shp_bridge_p1",
        "shp_bridge_p2",
        "shp_bridge_p3",
    ];
    if let Some(sa) = sprite_atlas {
        for (i, page) in sa.pages.iter().enumerate() {
            if let Some(key) = SHP_BRIDGE_KEYS.get(i) {
                if let Some((buf, count)) = pool.get(key) {
                    if count > 0 {
                        let instances = shp_paged.get(i).map_or(&[][..], Vec::as_slice);
                        groups.push(DrawGroup::new(&page.texture, buf, instances, count));
                    }
                }
            }
        }
    }

    if groups.is_empty() {
        return;
    }

    loop {
        let mut best_idx: Option<usize> = None;
        let mut best_depth: f32 = f32::NEG_INFINITY;
        for (i, group) in groups.iter().enumerate() {
            if group.cursor >= group.total {
                continue;
            }
            let depth = group.depth_at(group.cursor);
            if depth > best_depth {
                best_depth = depth;
                best_idx = Some(i);
            }
        }
        let Some(best_idx) = best_idx else { break };
        let start = groups[best_idx].cursor;
        let mut end = start + 1;
        while end < groups[best_idx].total {
            let depth = groups[best_idx].depth_at(end);
            if depth < best_depth {
                break;
            }
            end += 1;
        }
        batch.draw_depth_range(
            pass,
            groups[best_idx].texture,
            groups[best_idx].buffer,
            start,
            end - start,
        );
        groups[best_idx].cursor = end;
    }
}

/// Unified Y-sorted object pass: multi-way merge of VXL units, SHP entities, and walls.
///
/// All ground objects (buildings, infantry, vehicles, walls) are rendered in a
/// single Y-sorted pass (Layer 2). Walls render in both the terrain overlay pass AND
/// here -- the second rendering provides correct Y-sorted priority (walls in front of
/// units at closer iso rows). Our engine has multiple atlas textures, so we interleave
/// draw calls by walking cursors through each Y-sorted buffer and emitting sub-range draws.
pub(super) fn draw_merged_object_pass<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    batch: &'a BatchRenderer,
    pool: &'a InstanceBufferPool,
    unit_instances: &[SpriteInstance],
    shp_paged: &[Vec<SpriteInstance>],
    wall_instances: &[SpriteInstance],
    unit_atlas: Option<&'a UnitAtlas>,
    sprite_atlas: Option<&'a SpriteAtlas>,
    overlay_atlas: Option<&'a OverlayAtlas>,
) {
    // Each draw group has a bind group, pool buffer, extracted depth values, and cursor.
    // Depth values are extracted into Vec<f32> to avoid lifetime entanglement between
    // GPU resources (lifetime 'a) and CPU-side instance data (function params).
    // Sort is by depth DESCENDING (largest depth = furthest back = draw first).
    // Depth is based on iso_row (elevation-independent): GetYSort = X + Y
    // (which ignores Z elevation).
    let mut groups: Vec<DrawGroup<'a, '_>> = Vec::new();

    // VXL units draw group -- passthrough (no depth test).
    if let (Some(ua), Some((buf, count))) = (unit_atlas, pool.get("unit")) {
        if count > 0 {
            groups.push(DrawGroup::new(&ua.texture, buf, unit_instances, count));
        }
    }

    // SHP page draw groups — passthrough.
    const SHP_KEYS: [&str; 4] = ["shp_p0", "shp_p1", "shp_p2", "shp_p3"];
    if let Some(sa) = sprite_atlas {
        for (i, page) in sa.pages.iter().enumerate() {
            if let Some(key) = SHP_KEYS.get(i) {
                if let Some((buf, count)) = pool.get(key) {
                    if count > 0 {
                        let instances = shp_paged.get(i).map_or(&[][..], Vec::as_slice);
                        groups.push(DrawGroup::new(&page.texture, buf, instances, count));
                    }
                }
            }
        }
    }

    // Wall overlay draw group -- uses passthrough (no depth test).
    // Walls render in both terrain pass (overlays) and object pass (Layer 2).
    // The object pass rendering provides Y-sorted priority so walls appear
    // in front of units at closer iso rows.
    if let (Some(oa), Some((buf, count))) = (overlay_atlas, pool.get("overlay_wall")) {
        if count > 0 {
            groups.push(DrawGroup::new(&oa.texture, buf, wall_instances, count));
        }
    }

    if groups.is_empty() {
        return;
    }

    // Multi-way merge by depth DESCENDING: largest depth (furthest from camera)
    // draws first. Back-to-front rendering order based on GetYSort = X + Y,
    // which is elevation-independent.
    loop {
        // Find the group with the LARGEST current depth (furthest back).
        // At equal depth, prefer higher-index groups (SHP pages) over group 0 (VXL)
        // so buildings draw before VXL units at the same iso row.
        let mut best: Option<usize> = None;
        let mut best_d: f32 = -1.0;
        for (gi, g) in groups.iter().enumerate() {
            if g.cursor >= g.total {
                continue;
            }
            let d = g.depth_at(g.cursor);
            // Larger depth = further back = should draw first.
            // At equal depth, prefer SHP (gi > 0) over VXL (gi == 0).
            if d > best_d || (d == best_d && gi > 0) {
                best_d = d;
                best = Some(gi);
            }
        }
        let Some(gi) = best else { break };

        // Scan forward: how many consecutive instances from this group can we
        // draw before another group has a larger depth (needs to draw first)?
        let g = &groups[gi];
        let run_start = g.cursor;
        let mut run_end = run_start + 1;
        while run_end < g.total {
            let next_d = g.depth_at(run_end);
            // Check if any other group has a larger depth (further back, should draw first).
            let mut other_has_larger = false;
            for (oi, og) in groups.iter().enumerate() {
                if oi == gi || og.cursor >= og.total {
                    continue;
                }
                let other_d = og.depth_at(og.cursor);
                if other_d > next_d || (other_d == next_d && oi > gi) {
                    other_has_larger = true;
                    break;
                }
            }
            if other_has_larger {
                break;
            }
            run_end += 1;
        }

        // Draw the contiguous run -- all groups use passthrough (no depth test);
        // sprites never interact with the Z-buffer.
        let count = run_end - run_start;
        batch.draw_passthrough_range(
            pass,
            groups[gi].texture,
            groups[gi].buffer,
            run_start,
            count,
        );
        groups[gi].cursor = run_end;
    }
}
