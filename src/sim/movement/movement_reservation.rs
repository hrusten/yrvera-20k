//! Destination commitment — allocates infantry sub-cell or vehicle cell-center dest
//! after a successful cell transition. Previously also wrote to local reservation sets;
//! now the live OccupancyGrid is the single source of truth.

use crate::map::entities::EntityCategory;
use crate::sim::components::{MovementTarget, Position};
use crate::sim::movement::bump_crush;
use crate::sim::movement::drive_track::DriveTrackState;
use crate::sim::movement::locomotor::{LocomotorState, MovementLayer};
use crate::sim::occupancy::OccupancyGrid;
use crate::sim::rng::SimRng;

pub(super) fn reserve_destination_after_transition(
    category: EntityCategory,
    locomotor: &mut Option<LocomotorState>,
    drive_track: &mut Option<DriveTrackState>,
    position: &mut Position,
    sub_cell: &mut Option<u8>,
    target: &mut MovementTarget,
    next_layer: MovementLayer,
    nx: u16,
    ny: u16,
    occupancy: &OccupancyGrid,
    rng: &mut SimRng,
) -> bool {
    if category == EntityCategory::Infantry {
        let Some(sub) = bump_crush::allocate_sub_cell_with_preference(
            occupancy.get(nx, ny),
            next_layer,
            None,
            position.sub_x,
            position.sub_y,
            rng,
        ) else {
            position.sub_x = crate::util::lepton::CELL_CENTER_LEPTON;
            position.sub_y = crate::util::lepton::CELL_CENTER_LEPTON;
            *drive_track = None;
            target.movement_delay = 0;
            return false;
        };
        *sub_cell = Some(sub);
        if let Some(loco) = locomotor {
            let (dest_x, dest_y) = crate::util::lepton::subcell_lepton_offset(Some(sub));
            loco.subcell_dest = Some((dest_x, dest_y));
        }
    } else {
        if let Some(loco) = locomotor {
            loco.subcell_dest = Some((
                crate::util::lepton::CELL_CENTER_LEPTON,
                crate::util::lepton::CELL_CENTER_LEPTON,
            ));
        }
    }

    true
}
