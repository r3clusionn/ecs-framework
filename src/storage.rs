//! Archetype storage. Every distinct set of component types is an archetype: a table with one
//! column per component and one row per entity, so a query walks dense, same-typed arrays.
//!
//! A column is a type-erased vector (layout and drop function from [`ComponentInfo`]) with two
//! change ticks per row: when the value was added and when it was last changed.

use std::alloc::{self, Layout};
use std::any::TypeId;
use std::cell::UnsafeCell;
use std::ptr::NonNull;

use crate::component::{ComponentId, ComponentInfo};
use crate::entity::Entity;
use crate::hash::FxMap;

pub struct Column {
    item: Layout,
    drop: Option<unsafe fn(*mut u8)>,
    data: NonNull<u8>,
    len: usize,
    cap: usize,
    added: UnsafeCell<Vec<u32>>,
    changed: UnsafeCell<Vec<u32>>,
}

// Columns are only written through `&mut` or by systems the scheduler keeps apart (see `Access`).
unsafe impl Send for Column {}
unsafe impl Sync for Column {}

impl Column {
    pub fn new(info: &ComponentInfo) -> Column {
        Column {
            item: info.layout,
            drop: info.drop,
            // A zero-sized type never allocates; its pointer only needs to be aligned.
            data: NonNull::new(std::ptr::without_provenance_mut::<u8>(info.layout.align())).unwrap(),
            len: 0,
            cap: if info.layout.size() == 0 { usize::MAX } else { 0 },
            added: UnsafeCell::new(Vec::new()),
            changed: UnsafeCell::new(Vec::new()),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn array(&self, n: usize) -> Layout {
        let size = self.item.size().checked_mul(n).expect("column too large");
        Layout::from_size_align(size, self.item.align()).expect("column too large")
    }

    fn reserve_one(&mut self) {
        if self.len < self.cap {
            return;
        }
        self.grow_to((self.cap * 2).max(4));
    }

    /// Makes room for `n` more values.
    pub fn reserve(&mut self, n: usize) {
        if self.len + n > self.cap {
            self.grow_to((self.len + n).max(self.cap * 2));
        }
        self.added.get_mut().reserve(n);
        self.changed.get_mut().reserve(n);
    }

    fn grow_to(&mut self, new_cap: usize) {
        let new_layout = self.array(new_cap);
        let p = unsafe {
            if self.cap == 0 {
                alloc::alloc(new_layout)
            } else {
                alloc::realloc(self.data.as_ptr(), self.array(self.cap), new_layout.size())
            }
        };
        self.data = NonNull::new(p).unwrap_or_else(|| alloc::handle_alloc_error(new_layout));
        self.cap = new_cap;
    }

    /// Pointer to row `row`. Valid for `row <= len` (one past the end for writing).
    pub fn ptr(&self, row: usize) -> *mut u8 {
        unsafe { self.data.as_ptr().add(row * self.item.size()) }
    }

    /// Moves a value in (copies its bytes; the caller must not drop the source).
    ///
    /// # Safety
    /// `src` must point to a valid value of this column's type.
    pub unsafe fn push(&mut self, src: *const u8, tick: u32) {
        self.reserve_one();
        unsafe { std::ptr::copy_nonoverlapping(src, self.ptr(self.len), self.item.size()) };
        self.len += 1;
        self.added.get_mut().push(tick);
        self.changed.get_mut().push(tick);
    }

    /// Replaces the value at `row`, dropping the old one.
    ///
    /// # Safety
    /// `src` must point to a valid value of this column's type and `row < len`.
    pub unsafe fn replace(&mut self, row: usize, src: *const u8, tick: u32) {
        let dst = self.ptr(row);
        if let Some(d) = self.drop {
            unsafe { d(dst) };
        }
        unsafe { std::ptr::copy_nonoverlapping(src, dst, self.item.size()) };
        self.changed.get_mut()[row] = tick;
    }

    /// Removes row `row` by moving the last row into it. With `out`, the removed value is moved
    /// there; without, it is dropped.
    ///
    /// # Safety
    /// `row < len`; `out`, if given, must be valid for a write of one value.
    pub unsafe fn swap_remove(&mut self, row: usize, out: Option<*mut u8>) -> (u32, u32) {
        let last = self.len - 1;
        let p = self.ptr(row);
        match out {
            Some(o) => unsafe { std::ptr::copy_nonoverlapping(p, o, self.item.size()) },
            None => {
                if let Some(d) = self.drop {
                    unsafe { d(p) };
                }
            }
        }
        if row != last {
            unsafe { std::ptr::copy_nonoverlapping(self.ptr(last), p, self.item.size()) };
        }
        self.len -= 1;
        let a = self.added.get_mut().swap_remove(row);
        let c = self.changed.get_mut().swap_remove(row);
        (a, c)
    }

    /// Moves row `row` into `dst` (another column of the same type), keeping its ticks.
    ///
    /// # Safety
    /// `dst` must hold the same type and `row < len`.
    pub unsafe fn move_row(&mut self, row: usize, dst: &mut Column) {
        dst.reserve_one();
        let at = dst.len;
        let (a, c) = unsafe { self.swap_remove(row, Some(dst.ptr(at))) };
        dst.len += 1;
        dst.added.get_mut().push(a);
        dst.changed.get_mut().push(c);
    }

    pub fn added_ticks(&self) -> *mut u32 {
        unsafe { (&mut *self.added.get()).as_mut_ptr() }
    }

    pub fn changed_ticks(&self) -> *mut u32 {
        unsafe { (&mut *self.changed.get()).as_mut_ptr() }
    }

    pub fn ticks(&self, row: usize) -> (u32, u32) {
        unsafe { (*self.added_ticks().add(row), *self.changed_ticks().add(row)) }
    }
}

impl Drop for Column {
    fn drop(&mut self) {
        if let Some(d) = self.drop {
            for i in 0..self.len {
                unsafe { d(self.ptr(i)) };
            }
        }
        if self.item.size() > 0 && self.cap > 0 {
            unsafe { alloc::dealloc(self.data.as_ptr(), self.array(self.cap)) };
        }
    }
}

pub struct Archetype {
    pub id: u32,
    /// Sorted.
    pub components: Vec<ComponentId>,
    pub columns: Vec<Column>,
    pub entities: Vec<Entity>,
    /// Bundle type inserted here: the archetype the entity moves to and, per bundle element,
    /// its column there.
    pub add_edges: FxMap<TypeId, (u32, u32)>,
    pub remove_edges: FxMap<ComponentId, u32>,
}

impl Archetype {
    pub fn new(id: u32, components: Vec<ComponentId>, infos: &[&ComponentInfo]) -> Archetype {
        Archetype {
            id,
            columns: infos.iter().map(|i| Column::new(i)).collect(),
            components,
            entities: Vec::new(),
            add_edges: FxMap::default(),
            remove_edges: FxMap::default(),
        }
    }

    pub fn column_index(&self, c: ComponentId) -> Option<usize> {
        self.components.binary_search(&c).ok()
    }

    pub fn has(&self, c: ComponentId) -> bool {
        self.column_index(c).is_some()
    }

    pub fn column(&self, c: ComponentId) -> Option<&Column> {
        self.column_index(c).map(|i| &self.columns[i])
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DROPS: AtomicUsize = AtomicUsize::new(0);

    struct Counted(#[allow(dead_code)] u64);
    impl Drop for Counted {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn push_replace_remove_and_drop_each_value_once() {
        let info = ComponentInfo::of::<Counted>();
        let mut c = Column::new(&info);
        for i in 0..10u64 {
            let v = std::mem::ManuallyDrop::new(Counted(i));
            unsafe { c.push(&*v as *const Counted as *const u8, i as u32) };
        }
        let before = DROPS.load(Ordering::SeqCst);
        let v = std::mem::ManuallyDrop::new(Counted(99));
        unsafe { c.replace(3, &*v as *const Counted as *const u8, 50) };
        assert_eq!(DROPS.load(Ordering::SeqCst) - before, 1);
        assert_eq!(c.ticks(3), (3, 50));
        unsafe { c.swap_remove(0, None) };
        assert_eq!(DROPS.load(Ordering::SeqCst) - before, 2);
        // Row 0 now holds what was the last value.
        assert_eq!(unsafe { (*(c.ptr(0) as *const Counted)).0 }, 9);
        assert_eq!(c.len(), 9);
        drop(c);
        assert_eq!(DROPS.load(Ordering::SeqCst) - before, 11);
    }

    #[test]
    fn zero_sized_and_over_aligned_types() {
        #[derive(Clone, Copy)]
        #[repr(align(64))]
        struct Wide(#[allow(dead_code)] u8);
        let mut c = Column::new(&ComponentInfo::of::<Wide>());
        for i in 0..100 {
            unsafe { c.push(&Wide(i) as *const Wide as *const u8, 0) };
            assert_eq!(c.ptr(i as usize) as usize % 64, 0);
        }
        struct Marker;
        let mut z = Column::new(&ComponentInfo::of::<Marker>());
        for _ in 0..1000 {
            unsafe { z.push(&Marker as *const Marker as *const u8, 0) };
        }
        assert_eq!(z.len(), 1000);
        unsafe { z.swap_remove(5, None) };
        assert_eq!(z.len(), 999);
    }

    #[test]
    fn moving_rows_keeps_values_and_ticks() {
        let info = ComponentInfo::of::<String>();
        let (mut a, mut b) = (Column::new(&info), Column::new(&info));
        for (i, s) in ["x", "y", "z"].iter().enumerate() {
            let v = std::mem::ManuallyDrop::new(s.to_string());
            unsafe { a.push(&*v as *const String as *const u8, i as u32 + 1) };
        }
        unsafe { a.move_row(0, &mut b) };
        assert_eq!(unsafe { &*(b.ptr(0) as *const String) }, "x");
        assert_eq!(b.ticks(0), (1, 1));
        assert_eq!(unsafe { &*(a.ptr(0) as *const String) }, "z");
        assert_eq!(a.len(), 2);
    }
}
