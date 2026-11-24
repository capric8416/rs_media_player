use super::av1_error;
use crate::VideoResult;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReferenceSlot {
    pub(crate) frame_id: u32,
    pub(crate) order_hint: u32,
    pub(crate) frame_type: u8,
    pub(crate) surface_id: u64,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) render_width: u32,
    pub(crate) render_height: u32,
    pub(crate) film_grain: super::frame::Av1FilmGrainParams,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReferenceSlots {
    slots: [Option<ReferenceSlot>; 8],
}

impl ReferenceSlots {
    pub(crate) fn render_size_for_surface(&self, surface_id: u8) -> Option<(u32, u32)> {
        self.slots.iter().flatten().find_map(|slot| {
            (slot.surface_id == u64::from(surface_id))
                .then_some((slot.render_width, slot.render_height))
        })
    }

    pub(crate) fn surface_bindings(&self) -> VideoResult<Vec<(u64, u8)>> {
        let mut bindings = Vec::new();
        bindings
            .try_reserve_exact(self.slots.len())
            .map_err(|_| av1_error("ReferenceSlots::surface_bindings", "allocation failed"))?;
        for (index, slot) in self.slots.iter().enumerate() {
            if let Some(slot) = slot {
                let surface = u8::try_from(slot.surface_id).map_err(|_| {
                    av1_error(
                        "ReferenceSlots::surface_bindings",
                        "reference surface index exceeds u8",
                    )
                })?;
                bindings.push((
                    u64::try_from(index).map_err(|_| {
                        av1_error(
                            "ReferenceSlots::surface_bindings",
                            "reference slot index exceeds u64",
                        )
                    })?,
                    surface,
                ));
            }
        }
        Ok(bindings)
    }

    pub(crate) fn get(&self, index: u8) -> VideoResult<ReferenceSlot> {
        self.slots
            .get(usize::from(index))
            .and_then(|slot| *slot)
            .ok_or_else(|| av1_error("ReferenceSlots::get", "reference slot is unavailable"))
    }

    pub(crate) fn commit(&mut self, refresh_mask: u8, frame: ReferenceSlot) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if refresh_mask & (1_u8 << index) != 0 {
                *slot = Some(frame);
            }
        }
    }

    pub(crate) fn contains(&self, index: u8) -> bool {
        self.slots
            .get(usize::from(index))
            .is_some_and(Option::is_some)
    }

    pub(crate) fn can_overwrite_surface(&self, surface_id: u64, refresh_mask: u8) -> bool {
        self.slots.iter().enumerate().all(|(index, slot)| {
            slot.is_none_or(|reference| {
                reference.surface_id != surface_id || refresh_mask & (1_u8 << index) != 0
            })
        })
    }

    pub(crate) fn order_hint_or_zero(&self, index: usize) -> u32 {
        self.slots[index].map_or(0, |slot| slot.order_hint)
    }

    pub(crate) fn invalidate_stale(&mut self, current_id: u32, id_bits: u8, delta_bits: u8) {
        let modulus = 1_u32 << id_bits;
        let maximum_delta = 1_u32 << delta_bits;
        for slot in &mut self.slots {
            if let Some(reference) = slot {
                let delta = current_id.wrapping_sub(reference.frame_id) & (modulus - 1);
                if delta == 0 || delta >= maximum_delta {
                    *slot = None;
                }
            }
        }
    }

    pub(crate) fn invalidate_order_mismatch(&mut self, index: u8, order_hint: u32) {
        if let Some(slot) = self.slots.get_mut(usize::from(index))
            && slot.is_some_and(|reference| reference.order_hint != order_hint)
        {
            *slot = None;
        }
    }
}

/// Derives AV1's seven logical inter references from LAST and GOLDEN slots.
pub(crate) fn derive_short_refs(
    slots: &ReferenceSlots,
    current_hint: u32,
    hint_bits: u8,
    last: u8,
    golden: u8,
) -> VideoResult<[u8; 7]> {
    if hint_bits == 0 || hint_bits > 31 {
        return Err(av1_error(
            "derive_short_refs",
            "short signalling requires a valid order-hint width",
        ));
    }
    if last > 7 || golden > 7 || !slots.contains(last) || !slots.contains(golden) {
        return Err(av1_error(
            "derive_short_refs",
            "LAST and GOLDEN must name available reference slots",
        ));
    }

    let current = 1_i32 << (hint_bits - 1);
    let mut shifted = [0_i32; 8];
    for (index, hint) in shifted.iter_mut().enumerate() {
        *hint = current
            + relative_distance(
                true,
                hint_bits,
                slots.order_hint_or_zero(index),
                current_hint,
            );
    }
    let mut result = [u8::MAX; 7];
    result[0] = last;
    result[3] = golden;
    let mut used = [false; 8];
    used[usize::from(last)] = true;
    used[usize::from(golden)] = true;

    result[6] = select_forward(&shifted, used, current, true).unwrap_or(u8::MAX);
    mark_if_valid(&mut used, result[6]);
    result[4] = select_forward(&shifted, used, current, false).unwrap_or(u8::MAX);
    mark_if_valid(&mut used, result[4]);
    result[5] = select_forward(&shifted, used, current, false).unwrap_or(u8::MAX);
    mark_if_valid(&mut used, result[5]);

    for logical in [1_usize, 2, 4, 5, 6] {
        if result[logical] == u8::MAX
            && let Some(index) = select_backward(&shifted, used, current)
        {
            result[logical] = index;
            mark_if_valid(&mut used, index);
        }
    }
    let fallback = shifted
        .iter()
        .enumerate()
        .min_by_key(|(_, hint)| *hint)
        .map_or(last, |(index, _)| u8::try_from(index).unwrap_or(last));
    for index in &mut result {
        if *index == u8::MAX {
            *index = fallback;
        }
    }
    Ok(result)
}

fn select_forward(hints: &[i32; 8], used: [bool; 8], current: i32, latest: bool) -> Option<u8> {
    hints
        .iter()
        .enumerate()
        .filter(|(index, hint)| !used[*index] && **hint >= current)
        .min_by_key(|(_, hint)| if latest { -**hint } else { **hint })
        .and_then(|(index, _)| u8::try_from(index).ok())
}

fn select_backward(hints: &[i32; 8], used: [bool; 8], current: i32) -> Option<u8> {
    hints
        .iter()
        .enumerate()
        .filter(|(index, hint)| !used[*index] && **hint < current)
        .max_by_key(|(_, hint)| *hint)
        .and_then(|(index, _)| u8::try_from(index).ok())
}

fn mark_if_valid(used: &mut [bool; 8], index: u8) {
    if let Some(value) = used.get_mut(usize::from(index)) {
        *value = true;
    }
}

pub(crate) fn relative_distance(enabled: bool, bits: u8, first: u32, second: u32) -> i32 {
    if !enabled || bits == 0 {
        return 0;
    }
    let modulus = 1_i64 << bits;
    let mask = modulus - 1;
    let mut difference = (i64::from(first) - i64::from(second)) & mask;
    if difference & (modulus >> 1) != 0 {
        difference -= modulus;
    }
    i32::try_from(difference).unwrap_or_default()
}
