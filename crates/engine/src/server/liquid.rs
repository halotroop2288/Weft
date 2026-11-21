//! Liquids flow in the engine, following modern vanilla's FlowingFluid step by step: a liquid
//! block reacts to neighbor changes by scheduling a tick after its delay; a tick recomputes the
//! level from the neighbors, falls if it can and otherwise spreads sideways only towards the
//! nearest way down within `slope_find`. Mods describe liquids and reactions as data
//! (`abi::comp::liquid`, `abi::comp::reaction`), nothing calls into a mod per update.

use glam::{DVec3, IVec3};
use weft_abi::{ItemStack, block_flags as bf};

use super::dim::Dim;
use crate::content::{AIR, Content, LiquidDef};
use crate::world::DIRS;

/// North, east, south, west: vanilla's horizontal order, which decides ties.
const HORIZONTAL: [IVec3; 4] = [IVec3::new(0, 0, -1), IVec3::new(1, 0, 0), IVec3::new(0, 0, 1), IVec3::new(-1, 0, 0)];

const BUCKET_CAPACITY: u32 = 1000;
const MILLIBUCKETS_PER_LEVEL: u32 = 125;

fn bucket_fill_levels(stored_mb: u32, available_levels: u32) -> u32 {
    available_levels.min(BUCKET_CAPACITY.saturating_sub(stored_mb) / MILLIBUCKETS_PER_LEVEL)
}

fn bucket_pour_levels(stored_mb: u32, existing_levels: u32) -> u32 {
    (stored_mb / MILLIBUCKETS_PER_LEVEL).min(8u32.saturating_sub(existing_levels))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Fluid {
    id: u16,
    /// 1..8 units of liquid in this cell.
    amount: u32,
    source: bool,
}

fn decode(c: &Content, state: u32) -> Option<Fluid> {
    let id = c.states[state as usize].liquid?;
    let i = state - c.liquids[id as usize].first_state;
    let source = i == 0;
    Some(Fluid { id, amount: if source || !has_liquid_capacity(i) { 8 } else { 8 - i }, source })
}

fn encode(def: &LiquidDef, amount: u32) -> u32 {
    def.first_state + 8 - amount.clamp(1, 8)
}

/// Add incoming volume to a cell, returning its capped amount and the overflow.
fn has_liquid_capacity(amount: u32) -> bool {
    amount < 8
}

/// Add incoming volume to a cell, returning its capped amount and the overflow.
fn split_volume(existing: u32, incoming: u32) -> (u32, u32) {
    let total = existing + incoming;
    (total.min(8), total.saturating_sub(8))
}

/// Equal-density liquids need unequal volumes to establish an unambiguous layer order.
fn can_mix_by_density_and_volume(a_density: i32, a_amount: u32, b_density: i32, b_amount: u32) -> bool {
    a_density != b_density || a_amount != b_amount
}

/// Apply Dihydrogen Monoxide's one-unit horizontal equalization in the given neighbor order.
/// A negative neighbor level marks a cell which cannot receive this liquid.
fn equalize_levels(center: &mut u32, levels: &mut [i32; 4], order: [usize; 4], floor: u32) {
    loop {
        let mut moved = false;
        for i in order {
            if levels[i] >= 0 && *center > floor && *center >= levels[i] as u32 + 2 {
                *center -= 1;
                levels[i] += 1;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bucket_fill_levels, bucket_pour_levels, can_mix_by_density_and_volume, equalize_levels, has_liquid_capacity, split_volume,
    };

    #[test]
    fn bucket_fills_only_to_its_remaining_capacity() {
        assert_eq!(bucket_fill_levels(0, 3), 3);
        assert_eq!(bucket_fill_levels(875, 8), 1);
        assert_eq!(bucket_fill_levels(1000, 8), 0);
    }

    #[test]
    fn bucket_pours_only_into_available_cell_capacity() {
        assert_eq!(bucket_pour_levels(375, 6), 2);
        assert_eq!(bucket_pour_levels(1000, 3), 5);
        assert_eq!(bucket_pour_levels(100, 0), 0);
    }

    #[test]
    fn gravity_caps_destination_and_keeps_overflow() {
        assert_eq!(split_volume(0, 8), (8, 0));
        assert_eq!(split_volume(3, 8), (8, 3));
        assert_eq!(split_volume(7, 1), (8, 0));
    }

    #[test]
    fn horizontal_equalization_conserves_volume() {
        let mut center = 8;
        let mut levels = [0, 0, 0, 0];
        equalize_levels(&mut center, &mut levels, [0, 1, 2, 3], 0);
        assert_eq!(center + levels.iter().map(|&v| v as u32).sum::<u32>(), 8);
        assert_eq!(center, 1);
        assert_eq!(levels, [2, 2, 2, 1]);
    }

    #[test]
    fn equalization_ignores_unavailable_neighbors() {
        let mut center = 5;
        let mut levels = [-1, 0, -1, 1];
        equalize_levels(&mut center, &mut levels, [0, 1, 2, 3], 0);
        assert_eq!(
            center
                + levels
                    .iter()
                    .filter(|&&v| v >= 0)
                    .map(|&v| v as u32)
                    .sum::<u32>(),
            6
        );
        assert_eq!(levels, [-1, 2, -1, 2]);
        assert_eq!(center, 2);
    }

    #[test]
    fn one_open_neighbor_can_receive_the_whole_cell() {
        let mut center = 8;
        let mut levels = [-1, 0, -1, -1];
        equalize_levels(&mut center, &mut levels, [0, 1, 2, 3], 0);
        assert_eq!(center, 1);
        assert_eq!(levels, [-1, 7, -1, -1]);
    }

    #[test]
    fn horizontal_order_changes_ties_without_changing_total_volume() {
        let mut north_first = 8;
        let mut a = [0, 0, -1, -1];
        equalize_levels(&mut north_first, &mut a, [0, 1, 2, 3], 0);

        let mut east_first = 8;
        let mut b = [0, 0, -1, -1];
        equalize_levels(&mut east_first, &mut b, [1, 0, 2, 3], 0);

        assert_eq!(
            north_first
                + a.iter()
                    .filter(|&&v| v >= 0)
                    .map(|&v| v as u32)
                    .sum::<u32>(),
            8
        );
        assert_eq!(
            east_first
                + b.iter()
                    .filter(|&&v| v >= 0)
                    .map(|&v| v as u32)
                    .sum::<u32>(),
            8
        );
        assert_eq!(north_first, 3);
        assert_eq!(east_first, 3);
        assert_eq!(a[0], 3);
        assert_eq!(a[1], 2);
        assert_eq!(b[0], 2);
        assert_eq!(b[1], 3);
    }

    #[test]
    fn equalization_preserves_the_configured_drop_off() {
        let mut center = 8;
        let mut levels = [0, 0, 0, 0];
        equalize_levels(&mut center, &mut levels, [0, 1, 2, 3], 1);
        assert_eq!(center, 1);
        assert_eq!(center + levels.iter().map(|&v| v as u32).sum::<u32>(), 8);
    }

    #[test]
    fn compound_mixing_requires_an_unambiguous_layer_order() {
        assert!(!can_mix_by_density_and_volume(1000, 3, 1000, 3));
        assert!(can_mix_by_density_and_volume(1000, 2, 1000, 3));
        assert!(can_mix_by_density_and_volume(800, 3, 1000, 3));
    }

    #[test]
    fn full_liquid_cells_are_solid_but_partial_cells_can_receive() {
        assert!(!has_liquid_capacity(8));
        assert!(!has_liquid_capacity(9));
        assert!(has_liquid_capacity(7));
        assert!(has_liquid_capacity(1));
    }

    #[test]
    fn shuffled_horizontal_order_is_a_permutation() {
        let mut order = [0, 1, 2, 3];
        let mut seed = 0x1234_5678_9ABC_DEF0u64;
        for i in (1..order.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            order.swap(i, seed as usize % (i + 1));
        }
        order.sort_unstable();
        assert_eq!(order, [0, 1, 2, 3]);
    }
}

impl Dim {
    /// Every liquid constituent in a cell. A cell's capacity is shared by all constituents.
    fn fluids(&self, space_id: u64, c: &Content, p: IVec3) -> Vec<Fluid> {
        self.parts(space_id, p)
            .into_iter()
            .filter_map(|state| decode(c, state))
            .collect()
    }

    fn fluid(&self, space_id: u64, c: &Content, p: IVec3) -> Option<Fluid> {
        self.fluids(space_id, c, p).into_iter().next()
    }

    fn fluid_of(&self, space_id: u64, c: &Content, p: IVec3, id: u16) -> Option<Fluid> {
        self.fluids(space_id, c, p)
            .into_iter()
            .find(|fluid| fluid.id == id)
    }

    fn liquid_volume(&self, space_id: u64, c: &Content, p: IVec3) -> u32 {
        self.fluids(space_id, c, p).iter().map(|f| f.amount).sum()
    }

    /// Different liquids can share a cell unless their equal-density/equal-volume
    /// tie makes their vertical order ambiguous.
    fn can_mix(&self, c: &Content, incoming: Fluid, existing: Fluid) -> bool {
        if incoming.id == existing.id {
            return true;
        }
        let incoming_def = &c.liquids[incoming.id as usize];
        let existing_def = &c.liquids[existing.id as usize];
        let incoming_state = encode(incoming_def, incoming.amount);
        let existing_state = encode(existing_def, existing.amount);
        let reacts = |def: &LiquidDef, state| {
            def.falls_into
                .iter()
                .any(|&(matcher, _)| c.matchers.has(matcher, state))
                || def
                    .reacts_with
                    .iter()
                    .any(|&(matcher, _)| c.matchers.has(matcher, state))
        };
        !reacts(incoming_def, existing_state)
            && !reacts(existing_def, incoming_state)
            && can_mix_by_density_and_volume(incoming_def.density, incoming.amount, existing_def.density, existing.amount)
    }

    /// Keep the resident liquid order deterministic: lightest first, then the smaller
    /// constituent when densities tie. Non-liquid parts retain their relative positions.
    fn sort_liquid_parts(&mut self, space_id: u64, c: &'static Content, p: IVec3) -> bool {
        let Some(main) = self.get(space_id, p) else { return true };
        let mut parts: Vec<_> = self.space(space_id).layout(p).residents(main).collect();
        let mut liquids: Vec<_> = parts
            .iter()
            .copied()
            .filter(|part| c.states[part.state as usize].liquid.is_some())
            .collect();
        if liquids.is_empty() {
            return true;
        }
        liquids.sort_by_key(|part| {
            let fluid = decode(c, part.state).expect("liquid state must decode");
            (c.liquids[fluid.id as usize].density, fluid.amount, fluid.id)
        });
        let mut next = liquids.into_iter();
        for part in &mut parts {
            if c.states[part.state as usize].liquid.is_some() {
                *part = next.next().expect("same number of liquid parts");
            }
        }
        self.write_residents(space_id, c, p, &mut parts, false)
            .is_some()
    }

    /// Whether what shares the cell at `p` with a liquid closes its side towards `d`.
    fn sealed(&self, space_id: u64, c: &Content, p: IVec3, d: IVec3) -> bool {
        let space = self.space(space_id);
        let Some(main) = space.get(p) else { return false };
        let layout = space.layout(p);
        let Some(dir) = crate::world::DIRS.iter().position(|&v| v == d) else { return false };
        if layout.offsets.is_empty() && layout.covers.is_empty() && layout.extras.len() <= 1 {
            return std::iter::once(main)
                .chain(layout.extras.iter().copied())
                .any(|s| c.liquid_walls[s as usize] & (1 << dir) != 0);
        }
        layout.covered_faces(c, main, true) & (1 << dir) != 0
    }

    /// Room for liquid: neither solid nor a ladder (unloaded counts as solid).
    fn has_room(&self, space_id: u64, c: &Content, p: IVec3) -> bool {
        let room = |s: u32| c.states[s as usize].flags & (bf::SOLID | bf::CLIMBABLE) == 0;
        self.get(space_id, p).is_some_and(|main| {
            self.space(space_id)
                .layout(p)
                .parts(main)
                .all(|part| room(part.state))
        })
    }

    /// Blocks which explicitly share their cell with a liquid are fluid containers, not flow
    /// destinations. This mirrors Minecraft's FluidFillable exclusion.
    fn is_fluid_container(&self, space_id: u64, c: &Content, p: IVec3, l: u16) -> bool {
        let source = c.liquids[l as usize].first_state;
        self.parts(space_id, p)
            .into_iter()
            .any(|s| c.states[s as usize].liquid.is_none() && c.may_share(s, source))
    }

    /// Whether a cell can receive this liquid without displacing a constituent.
    fn can_flow_into(&self, space_id: u64, c: &Content, p: IVec3, l: u16, incoming_amount: u32) -> bool {
        if self
            .space(space_id)
            .bounds
            .is_some_and(|(lo, hi)| p.cmplt(lo).any() || p.cmpgt(hi).any())
            || self.is_fluid_container(space_id, c, p, l)
        {
            return false;
        }
        let existing = self.fluids(space_id, c, p);
        if existing.is_empty() {
            return self.has_room(space_id, c, p);
        }
        let total: u32 = existing.iter().map(|f| f.amount).sum();
        let combined = incoming_amount
            + existing
                .iter()
                .find(|fluid| fluid.id == l)
                .map_or(0, |fluid| fluid.amount);
        let incoming = Fluid { id: l, amount: combined, source: false };
        total.saturating_add(incoming_amount) <= 8
            && existing
                .into_iter()
                .filter(|other| other.id != l)
                .all(|other| self.can_mix(c, incoming, other))
    }

    fn spread_to(&mut self, space_id: u64, c: &'static Content, p: IVec3, state: u32) -> Option<()> {
        if self.fluid(space_id, c, p).is_some() || self.is_fluid_container(space_id, c, p, c.states[state as usize].liquid?) {
            return None;
        }
        let was = self.parts(space_id, p);
        self.set(space_id, c, p, state)?;
        // Plants and torches wash away with their drops, like vanilla.
        for old in was {
            self.drop_block_loot(space_id, c, p, old, ItemStack::EMPTY, false, 0);
        }
        Some(())
    }

    /// Write one constituent's amount without replacing the other liquids in the cell.
    fn set_liquid_level(&mut self, space_id: u64, c: &'static Content, p: IVec3, l: u16, amount: u32) -> bool {
        let old = self
            .parts(space_id, p)
            .into_iter()
            .find(|&state| c.states[state as usize].liquid == Some(l));
        let changed = match (old, amount) {
            (Some(old), 0) => self.replace_part(space_id, c, p, old, AIR),
            (Some(old), amount) => self.replace_part(space_id, c, p, old, encode(&c.liquids[l as usize], amount)),
            (None, 0) => true,
            (None, amount) => self.add_part(space_id, c, p, encode(&c.liquids[l as usize], amount)),
        };
        changed && self.sort_liquid_parts(space_id, c, p)
    }

    /// A full liquid cell is solid for slope searches.
    /// A partially filled cell can still receive volume and therefore remains a valid passage.
    fn is_hole(&self, space_id: u64, c: &Content, p: IVec3, l: u16, down: IVec3) -> bool {
        !self.sealed(space_id, c, p, down) && self.can_flow_into(space_id, c, p, l, 1)
    }

    /// Shortest horizontal distance from this cell to a cell with room in the gravity direction.
    /// The previous direction prevents immediately walking back along the edge we arrived through.
    fn slope_distance(&self, space_id: u64, c: &Content, p: IVec3, depth: u32, from: IVec3, l: u16, down: IVec3) -> u32 {
        let mut min = u32::MAX;
        for d in HORIZONTAL {
            if d == from {
                continue;
            }
            let next = p + d;
            if !self.can_flow_into(space_id, c, next, l, 1) || self.sealed(space_id, c, p, d) {
                continue;
            }
            if self.is_hole(space_id, c, next + down, l, down) {
                return depth;
            }
            if depth < c.liquids[l as usize].slope_find {
                min = min.min(self.slope_distance(space_id, c, next, depth + 1, -d, l, down));
            }
        }
        min
    }

    /// Keep only directions which lead to the nearest drop-off. If no drop-off is reachable
    /// within slope_find, return all open directions so the surface can level out.
    fn slope_directions(&self, space_id: u64, c: &Content, p: IVec3, l: u16, down: IVec3) -> [bool; 4] {
        let mut distances = [u32::MAX; 4];
        let mut open = [false; 4];
        let mut nearest = u32::MAX;
        for (i, d) in HORIZONTAL.into_iter().enumerate() {
            let next = p + d;
            if self.sealed(space_id, c, p, d) || !self.can_flow_into(space_id, c, next, l, 1) {
                continue;
            }
            open[i] = true;
            let distance =
                if self.is_hole(space_id, c, next + down, l, down) { 0 } else { self.slope_distance(space_id, c, next, 1, -d, l, down) };
            distances[i] = distance;
            nearest = nearest.min(distance);
        }

        // MAX means no reachable drop-off, not a closed direction. Keep every open
        // neighbor so a trapped full cell can level out.
        if nearest == u32::MAX {
            return open;
        }
        distances.map(|distance| distance == nearest)
    }

    /// A deterministic shuffle gives each tick an unbiased horizontal order without making
    /// simulation results depend on thread scheduling or a global random-number generator.
    fn shuffled_horizontal(&self, p: IVec3) -> [usize; 4] {
        let mut order = [0, 1, 2, 3];
        let mut seed = self.time
            ^ (p.x as u32 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (p.y as u32 as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
            ^ (p.z as u32 as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
        if seed == 0 {
            seed = 0xA076_1D64_78BD_642F;
        }
        for i in (1..order.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            order.swap(i, seed as usize % (i + 1));
        }
        order
    }

    /// The scheduled tick of a liquid block.
    pub(super) fn liquid_tick(&mut self, space_id: u64, c: &'static Content, p: IVec3, state: u32) {
        let Some(f) = decode(c, state) else { return };
        if self.is_fluid_container(space_id, c, p, f.id) {
            return;
        }

        let down = if c.liquids[f.id as usize].rises { IVec3::Y } else { IVec3::NEG_Y };
        let below = p + down;

        // Gravity transfers the entire cell's volume into the next cell. If that cell overflows,
        // the excess remains in the cell the liquid came from.
        let below_total = self.liquid_volume(space_id, c, below);
        let incoming = f.amount.min(8u32.saturating_sub(below_total));
        if incoming > 0 && !self.sealed(space_id, c, p, down) && self.can_flow_into(space_id, c, below, f.id, incoming) {
            let previous_below = self.parts(space_id, below);
            let existing = self
                .fluid_of(space_id, c, below, f.id)
                .map_or(0, |n| n.amount);
            let (next_below, overflow) = split_volume(existing, incoming);
            let wrote_below = if existing > 0 {
                self.set_liquid_level(space_id, c, below, f.id, next_below)
            } else {
                self.add_part(space_id, c, below, encode(&c.liquids[f.id as usize], incoming)) && self.sort_liquid_parts(space_id, c, below)
            };
            if wrote_below && !self.set_liquid_level(space_id, c, p, f.id, f.amount - incoming + overflow) {
                // Restore the entire destination layout, not just its topmost liquid.
                let first = previous_below.first().copied().unwrap_or(AIR);
                self.write_cell(space_id, c, below, first, Some(previous_below.into_iter().skip(1).collect()), false);
            }
            return;
        }

        // Prefer the shortest route to a drop-off. If no route exists within slope_find,
        // equalize with every open horizontal neighbor to produce a level surface.
        let directions = self.slope_directions(space_id, c, p, f.id, down);
        let mut levels = [-1; 4];
        for (i, d) in HORIZONTAL.into_iter().enumerate() {
            if directions[i] {
                levels[i] = self
                    .fluid_of(space_id, c, p + d, f.id)
                    .map_or(0, |n| n.amount) as i32;
            }
        }
        let before = levels;
        let mut desired_center = f.amount;
        equalize_levels(&mut desired_center, &mut levels, self.shuffled_horizontal(p), 0);

        let mut moved: Vec<(IVec3, Vec<u32>)> = Vec::new();
        let mut center = f.amount;
        let mut all_neighbors_written = true;
        for (i, d) in HORIZONTAL.into_iter().enumerate() {
            if levels[i] >= 0 && levels[i] != before[i] {
                let at = p + d;
                let previous = self.parts(space_id, at);
                let amount = (levels[i] - before[i]) as u32;
                if self.set_liquid_level(space_id, c, at, f.id, levels[i] as u32) {
                    center -= amount;
                    moved.push((at, previous));
                } else {
                    all_neighbors_written = false;
                }
            }
        }
        if all_neighbors_written {
            center = desired_center;
        }
        if center != f.amount && !self.set_liquid_level(space_id, c, p, f.id, center) {
            // Restore complete layouts, including every constituent and non-liquid part.
            for (at, previous) in moved {
                let first = previous.first().copied().unwrap_or(AIR);
                self.write_cell(space_id, c, at, first, Some(previous.into_iter().skip(1).collect()), false);
            }
        }
    }

    /// Consume one unit of a falling liquid when its configured reaction turns the
    /// touched block into a result state. None means this state/result is not a falls-into
    /// reaction; Some(false) means the reaction source was unavailable.
    pub(super) fn consume_falling_reaction(
        &mut self,
        space_id: u64,
        c: &'static Content,
        p: IVec3,
        touching_state: u32,
        result: u32,
    ) -> Option<bool> {
        for (id, liquid) in c.liquids.iter().enumerate() {
            if !liquid
                .falls_into
                .iter()
                .any(|&(matcher, into)| into == result && c.matchers.has(matcher, touching_state))
            {
                continue;
            }

            let down = if liquid.rises { IVec3::Y } else { IVec3::NEG_Y };
            let source_pos = p - down;
            let Some(source) = self.fluid_of(space_id, c, source_pos, id as u16) else {
                return Some(false);
            };
            if source.id as usize != id {
                continue;
            }
            if !self.replace_part(space_id, c, p, touching_state, result) {
                return Some(false);
            }
            if !self.set_liquid_level(space_id, c, source_pos, source.id, source.amount.saturating_sub(1)) {
                self.replace_part(space_id, c, p, result, touching_state);
                return Some(false);
            }
            return Some(true);
        }

        // Horizontal/side-contact reactions consume one unit of the liquid being touched.
        // The reacting liquid at p is transformed; the adjacent liquid supplies the cost.
        for (liquid_id, liquid) in c.liquids.iter().enumerate() {
            for &(matcher, into) in &liquid.reacts_with {
                if into != result {
                    continue;
                }
                for d in DIRS {
                    let water_pos = p + d;
                    let Some(water_state) = self.parts(space_id, water_pos).into_iter().find(|&state| {
                        c.states[state as usize]
                            .liquid
                            .is_some_and(|id| id as usize == liquid_id)
                    }) else {
                        continue;
                    };
                    if !c.matchers.has(matcher, water_state) {
                        continue;
                    }
                    let Some(water) = decode(c, water_state) else {
                        continue;
                    };
                    if !self.replace_part(space_id, c, p, touching_state, result) {
                        return Some(false);
                    }
                    if !self.set_liquid_level(space_id, c, water_pos, water.id, water.amount.saturating_sub(1)) {
                        self.replace_part(space_id, c, p, result, touching_state);
                        return Some(false);
                    }
                    return Some(true);
                }
                return Some(false);
            }
        }
        None
    }

    /// A neighbor of a liquid changed: react with what touches it, else flow after the delay.
    pub(super) fn liquid_neighbor(&mut self, space_id: u64, c: &'static Content, p: IVec3, state: u32) {
        let Some(f) = decode(c, state) else { return };
        let def = &c.liquids[f.id as usize];
        self.schedule(space_id, p, def.delay);
    }

    /// Transfer liquid between a stack-backed bucket and the targeted cell. Ordinary use fills;
    /// sneaking use pours. The new held stack, or `None` if nothing changed.
    pub fn use_container(&mut self, space_id: u64, c: &'static Content, player: u64, held: ItemStack) -> Option<ItemStack> {
        if held.is_empty() {
            return None;
        }
        let marker = c.components.id(weft_abi::comp::item::BUCKET)?;
        if !c.components.has(crate::components::ITEM, held.item, marker) {
            return None;
        }
        let reach = self.block_reach(space_id, c, player);
        let flags = weft_abi::ray::BLOCKS | weft_abi::ray::LIQUIDS;
        let hit = self.look_ray(space_id, c, player, reach, flags)?;
        self.with_space(c, hit.frame, |region, _, _| region.use_bucket_at(c, player, held, hit))?
    }

    fn use_bucket_at(&mut self, c: &'static Content, player: u64, held: ItemStack, hit: crate::physics::Hit) -> Option<ItemStack> {
        let space_id = hit.frame;
        let maybe_player = self.entities.get(player);
        let creative = maybe_player.is_some_and(|e| e.mode.has(crate::tag_bits::game_mode::NO_COSTS));
        let sneak = maybe_player.is_some_and(|e| e.pose.eq(&crate::world::Pose::CROUCH));
        let liquid_component = c.components.id(weft_abi::comp::BUCKET_LIQUID)?;
        let amount_component = c.components.id(weft_abi::comp::BUCKET_AMOUNT)?;
        let texture_component = c.components.id(weft_abi::comp::BUCKET_TEXTURE)?;
        let stored_liquid = crate::stack::value(held.data, liquid_component)
            .filter(|b| b.len() >= 4)
            .map(|b| u32::from_le_bytes(b[..4].try_into().unwrap()) as u16);
        let stored_amount = crate::stack::u32_of(&held, Some(amount_component)).min(BUCKET_CAPACITY);

        let set_contents = |stack: ItemStack, liquid: Option<u16>, amount: u32, texture: Option<&str>| {
            let mut data = stack.data;
            if let Some(liquid) = liquid {
                data = crate::stack::with(data, liquid_component, Some(&(liquid as u32).to_le_bytes()));
            } else {
                data = crate::stack::with(data, liquid_component, None);
            }
            if amount == 0 {
                data = crate::stack::with(data, amount_component, None);
                data = crate::stack::with(data, texture_component, None);
            } else {
                data = crate::stack::with(data, amount_component, Some(&amount.to_le_bytes()));
                let mut bytes = Vec::new();
                weft_abi::data::write_text(texture.unwrap_or_default(), &mut bytes);
                data = crate::stack::with(data, texture_component, Some(&bytes));
            }
            ItemStack { data, ..stack }
        };

        if !sneak {
            if stored_amount >= BUCKET_CAPACITY {
                return None;
            }
            let fluids = self.fluids(space_id, c, hit.block);
            let fluid = if stored_amount > 0 {
                fluids
                    .into_iter()
                    .find(|fluid| Some(fluid.id) == stored_liquid)?
            } else {
                fluids
                    .into_iter()
                    .min_by_key(|fluid| (c.liquids[fluid.id as usize].density, fluid.amount, fluid.id))?
            };
            let levels = bucket_fill_levels(stored_amount, fluid.amount);
            if levels == 0 {
                return None;
            }
            let state = encode(&c.liquids[fluid.id as usize], fluid.amount);
            let mut q =
                weft_abi::Query { entity: player, pos: hit.block.to_array().into(), state, item: held.item, ..weft_abi::Query::DEFAULT };
            if !self.ask(space_id, c, weft_abi::action::BREAK_BLOCK, &mut q) {
                return None;
            }
            if !creative {
                let remaining = fluid.amount - levels;
                let next = if remaining == 0 { AIR } else { encode(&c.liquids[fluid.id as usize], remaining) };
                if !self.replace_part(space_id, c, hit.block, state, next) {
                    return None;
                }
                self.sort_liquid_parts(space_id, c, hit.block);
            }
            let texture = &c.liquids[fluid.id as usize].bucket_texture;
            return Some(set_contents(held, Some(fluid.id), stored_amount + levels * MILLIBUCKETS_PER_LEVEL, Some(texture.as_str())));
        }

        let liquid = stored_liquid?;
        if stored_amount == 0 {
            return None;
        }
        let replaceable = |state: u32| c.states[state as usize].flags & bf::REPLACEABLE != 0;
        let target = if c.states[hit.state as usize].liquid.is_some() || replaceable(hit.state) {
            hit.block
        } else {
            hit.block + DIRS[hit.face as usize]
        };
        if !self.can_hold_liquid(space_id, c, target, c.liquids[liquid as usize].first_state) {
            return None;
        }
        let existing = self.fluids(space_id, c, target);
        let used_levels = bucket_pour_levels(stored_amount, existing.iter().map(|f| f.amount).sum());
        if used_levels == 0 {
            return None;
        }
        let incoming = Fluid { id: liquid, amount: used_levels, source: used_levels == 8 };
        if existing
            .iter()
            .any(|other| !self.can_mix(c, incoming, *other))
        {
            return None;
        }
        let state = encode(&c.liquids[liquid as usize], used_levels);
        let mut q = weft_abi::Query { entity: player, pos: target.to_array().into(), state, item: held.item, ..weft_abi::Query::DEFAULT };
        if !self.ask(space_id, c, weft_abi::action::PLACE_BLOCK, &mut q) {
            return None;
        }
        if let Some(old) = existing.iter().find(|f| f.id == liquid).copied() {
            let next = encode(&c.liquids[liquid as usize], old.amount + used_levels);
            if !self.replace_part(space_id, c, target, encode(&c.liquids[liquid as usize], old.amount), next) {
                return None;
            }
        } else if !self.add_part(space_id, c, target, state) {
            return None;
        }
        self.sort_liquid_parts(space_id, c, target);
        let remaining = if creative { stored_amount } else { stored_amount - used_levels * MILLIBUCKETS_PER_LEVEL };
        Some(set_contents(
            held,
            (remaining > 0).then_some(liquid),
            remaining,
            (remaining > 0).then(|| crate::stack::text(held, weft_abi::comp::BUCKET_TEXTURE).unwrap_or_default()),
        ))
    }

    /// The block an entity looks at from its eyes, in the world or a moving structure.
    fn look_ray(&self, space_id: u64, c: &Content, id: u64, max: f64, flags: u32) -> Option<crate::physics::Hit> {
        let e = self.entities.get(id)?;
        let eye = e.pos + DVec3::Y * e.eye(c);
        let place = self.space_pose(space_id);
        self.ray(space_id, c, place.to_local(eye), place.vector_to_local(crate::physics::look_dir(e.yaw, e.pitch)), max, flags)
    }

    /// The first block along a ray from `from` (in this dimension's frame: a body's blocks when
    /// asked from them), bodies included when asked from the world; `flags` as `physics::pick`'s.
    pub fn ray(&self, space_id: u64, c: &Content, from: DVec3, dir: DVec3, max: f64, flags: u32) -> Option<crate::physics::Hit> {
        // Asked from a body's blocks, it looks along those.
        if space_id != 0 {
            return crate::physics::pick(c, self.space(space_id), &[], from, dir, max, flags)
                .map(|hit| crate::physics::Hit { frame: space_id, ..hit });
        }
        let structures: Vec<(u64, crate::physics::Transform, &dyn crate::physics::Blocks)> = self
            .bodies
            .iter()
            .filter_map(|(&sid, s)| {
                self.entities
                    .get(sid)
                    .map(|b| (sid, b.body_place(), &**s as &dyn crate::physics::Blocks))
            })
            .collect();
        crate::physics::pick(c, self.space(space_id), &structures, from, dir, max, flags)
    }
}
