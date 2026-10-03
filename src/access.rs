//! What a query or system touches: components read and written, components it requires or
//! excludes (which makes two queries disjoint even if they name the same type), and resources.
//! The scheduler runs two systems at once only if their accesses do not conflict.

use std::any::TypeId;
use std::collections::BTreeSet;

use crate::component::ComponentId;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BitSet(Vec<u64>);

impl BitSet {
    pub fn insert(&mut self, i: usize) {
        if self.0.len() <= i / 64 {
            self.0.resize(i / 64 + 1, 0);
        }
        self.0[i / 64] |= 1 << (i % 64);
    }

    pub fn contains(&self, i: usize) -> bool {
        self.0.get(i / 64).is_some_and(|w| w & (1 << (i % 64)) != 0)
    }

    pub fn intersects(&self, o: &BitSet) -> bool {
        self.0.iter().zip(&o.0).any(|(a, b)| a & b != 0)
    }

    pub fn union_with(&mut self, o: &BitSet) {
        if self.0.len() < o.0.len() {
            self.0.resize(o.0.len(), 0);
        }
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            *a |= b;
        }
    }

    pub fn ones(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(w, bits)| (0..64).filter(move |b| bits & (1 << b) != 0).map(move |b| w * 64 + b))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Access {
    pub reads: BitSet,
    pub writes: BitSet,
    /// Components every matched archetype has.
    pub required: BitSet,
    /// Components no matched archetype has.
    pub excluded: BitSet,
    pub res_reads: BTreeSet<TypeIdKey>,
    pub res_writes: BTreeSet<TypeIdKey>,
    /// Needs the whole world (an exclusive system).
    pub exclusive: bool,
}

/// `TypeId` with a stable order, for the resource sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeIdKey(pub TypeId);

impl Access {
    pub fn read(&mut self, c: ComponentId) {
        self.reads.insert(c.index());
    }

    pub fn write(&mut self, c: ComponentId) {
        self.writes.insert(c.index());
    }

    pub fn require(&mut self, c: ComponentId) {
        self.required.insert(c.index());
    }

    pub fn exclude(&mut self, c: ComponentId) {
        self.excluded.insert(c.index());
    }

    /// No archetype can match both.
    pub fn disjoint(&self, o: &Access) -> bool {
        self.required.intersects(&o.excluded) || o.required.intersects(&self.excluded)
    }

    pub fn conflicts(&self, o: &Access) -> bool {
        if self.exclusive || o.exclusive {
            return true;
        }
        let res = self.res_writes.iter().any(|r| o.res_reads.contains(r) || o.res_writes.contains(r))
            || o.res_writes.iter().any(|r| self.res_reads.contains(r));
        if res {
            return true;
        }
        let comp = self.writes.intersects(&o.reads) || self.writes.intersects(&o.writes) || o.writes.intersects(&self.reads);
        comp && !self.disjoint(o)
    }

    /// Adds another access of the same query (a tuple element). Panics on aliasing inside it.
    pub fn merge_checked(&mut self, o: &Access, what: &str) {
        if self.writes.intersects(&o.reads) || self.writes.intersects(&o.writes) || o.writes.intersects(&self.reads) {
            panic!("{what} accesses the same component mutably and another way at once");
        }
        self.reads.union_with(&o.reads);
        self.writes.union_with(&o.writes);
        self.required.union_with(&o.required);
        self.excluded.union_with(&o.excluded);
        self.res_reads.extend(o.res_reads.iter().copied());
        self.res_writes.extend(o.res_writes.iter().copied());
        self.exclusive |= o.exclusive;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(i: u32) -> ComponentId {
        ComponentId(i)
    }

    #[test]
    fn bitsets() {
        let mut a = BitSet::default();
        a.insert(3);
        a.insert(130);
        assert!(a.contains(130) && !a.contains(4));
        let mut b = BitSet::default();
        b.insert(130);
        assert!(a.intersects(&b));
        assert_eq!(a.ones().collect::<Vec<_>>(), vec![3, 130]);
    }

    #[test]
    fn reads_share_writes_do_not_unless_disjoint() {
        let mut r = Access::default();
        r.read(c(0));
        r.require(c(0));
        let mut r2 = r.clone();
        assert!(!r.conflicts(&r2));
        r2.write(c(0));
        assert!(r.conflicts(&r2));
        // One requires component 1, the other excludes it: never the same entities.
        r.require(c(1));
        r2.exclude(c(1));
        assert!(!r.conflicts(&r2));
        // The same with the roles swapped: the first excludes what the second requires.
        let mut x = Access::default();
        x.write(c(5));
        x.exclude(c(6));
        let mut y = Access::default();
        y.read(c(5));
        y.require(c(6));
        assert!(!x.conflicts(&y) && !y.conflicts(&x));
        let ex = Access { exclusive: true, ..Default::default() };
        assert!(ex.conflicts(&Access::default()));
    }

    #[test]
    #[should_panic(expected = "mutably")]
    fn aliasing_inside_one_query_panics() {
        let mut a = Access::default();
        a.write(c(2));
        let mut b = Access::default();
        b.read(c(2));
        a.merge_checked(&b, "the query");
    }
}
