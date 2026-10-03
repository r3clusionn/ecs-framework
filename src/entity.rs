//! Entities: an index into the world's tables plus a generation, so an id that outlives its
//! entity never refers to whatever reuses the slot.

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Entity {
    index: u32,
    generation: u32,
}

impl Entity {
    pub fn index(self) -> u32 {
        self.index
    }

    pub fn generation(self) -> u32 {
        self.generation
    }

    /// Packs the id into 64 bits (generation high, index low), for hashing or storing elsewhere.
    pub fn to_bits(self) -> u64 {
        ((self.generation as u64) << 32) | self.index as u64
    }

    pub fn from_bits(bits: u64) -> Entity {
        Entity { index: bits as u32, generation: (bits >> 32) as u32 }
    }
}

impl fmt::Debug for Entity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}v{}", self.index, self.generation)
    }
}

/// Where an entity's components are: its archetype and its row there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    pub archetype: u32,
    pub row: u32,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    generation: u32,
    location: Option<Location>,
}

/// Allocates entity ids and remembers where each live entity is stored.
#[derive(Default)]
pub struct Entities {
    slots: Vec<Slot>,
    free: Vec<u32>,
    live: usize,
}

impl Entities {
    pub fn alloc(&mut self) -> Entity {
        self.live += 1;
        match self.free.pop() {
            Some(index) => Entity { index, generation: self.slots[index as usize].generation },
            None => {
                let index = u32::try_from(self.slots.len()).expect("more than 4 billion entities");
                self.slots.push(Slot { generation: 0, location: None });
                Entity { index, generation: 0 }
            }
        }
    }

    /// Frees the id; its slot is reused with the next generation. Returns where it was stored.
    pub fn free(&mut self, e: Entity) -> Option<Location> {
        let slot = self.slots.get_mut(e.index as usize)?;
        if slot.generation != e.generation || slot.location.is_none() {
            return None;
        }
        let loc = slot.location.take();
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(e.index);
        self.live -= 1;
        loc
    }

    pub fn contains(&self, e: Entity) -> bool {
        self.location(e).is_some()
    }

    pub fn location(&self, e: Entity) -> Option<Location> {
        let s = self.slots.get(e.index as usize)?;
        if s.generation == e.generation {
            s.location
        } else {
            None
        }
    }

    pub fn set_location(&mut self, e: Entity, loc: Location) {
        let s = &mut self.slots[e.index as usize];
        debug_assert_eq!(s.generation, e.generation);
        s.location = Some(loc);
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freed_ids_are_reused_with_a_new_generation() {
        let mut e = Entities::default();
        let a = e.alloc();
        e.set_location(a, Location { archetype: 0, row: 0 });
        assert!(e.contains(a));
        assert_eq!(e.free(a), Some(Location { archetype: 0, row: 0 }));
        assert!(!e.contains(a));
        assert_eq!(e.free(a), None, "double free is refused");
        let b = e.alloc();
        assert_eq!(b.index(), a.index());
        assert_eq!(b.generation(), a.generation() + 1);
        e.set_location(b, Location { archetype: 1, row: 3 });
        assert!(!e.contains(a), "the stale id does not see the new entity");
        assert_eq!(e.len(), 1);
    }

    #[test]
    fn bits_round_trip() {
        let mut e = Entities::default();
        let a = e.alloc();
        let b = e.alloc();
        assert_eq!(Entity::from_bits(b.to_bits()), b);
        assert_ne!(a.to_bits(), b.to_bits());
        assert_eq!(format!("{b:?}"), "1v0");
    }
}
