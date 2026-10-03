//! Component types: any `'static + Send + Sync` type. Each gets a small id the first time a world
//! sees it, with the layout and drop function its type-erased column needs.

use std::alloc::Layout;
use std::any::TypeId;

/// Marker for types that can be components. Implemented for every `Send + Sync + 'static` type.
pub trait Component: Send + Sync + 'static {}
impl<T: Send + Sync + 'static> Component for T {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ComponentId(pub(crate) u32);

impl ComponentId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Debug)]
pub struct ComponentInfo {
    pub name: &'static str,
    pub type_id: TypeId,
    pub layout: Layout,
    /// Drops one value in place; `None` for types without drop glue.
    pub drop: Option<unsafe fn(*mut u8)>,
}

unsafe fn drop_ptr<T>(p: *mut u8) {
    unsafe { std::ptr::drop_in_place(p as *mut T) }
}

impl ComponentInfo {
    pub fn of<T: Component>() -> ComponentInfo {
        ComponentInfo {
            name: std::any::type_name::<T>(),
            type_id: TypeId::of::<T>(),
            layout: Layout::new::<T>(),
            drop: if std::mem::needs_drop::<T>() { Some(drop_ptr::<T>) } else { None },
        }
    }
}

#[derive(Default)]
pub struct Components {
    infos: Vec<ComponentInfo>,
    by_type: crate::hash::FxMap<TypeId, ComponentId>,
}

impl Components {
    pub fn id<T: Component>(&mut self) -> ComponentId {
        let t = TypeId::of::<T>();
        if let Some(id) = self.by_type.get(&t) {
            return *id;
        }
        let id = ComponentId(self.infos.len() as u32);
        self.infos.push(ComponentInfo::of::<T>());
        self.by_type.insert(t, id);
        id
    }

    pub fn get_id<T: Component>(&self) -> Option<ComponentId> {
        self.by_type.get(&TypeId::of::<T>()).copied()
    }

    pub fn info(&self, id: ComponentId) -> &ComponentInfo {
        &self.infos[id.index()]
    }

    pub fn len(&self) -> usize {
        self.infos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.infos.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_per_type() {
        let mut c = Components::default();
        let a = c.id::<u32>();
        let b = c.id::<String>();
        assert_ne!(a, b);
        assert_eq!(c.id::<u32>(), a);
        assert_eq!(c.get_id::<String>(), Some(b));
        assert_eq!(c.get_id::<u8>(), None);
        assert!(c.info(a).drop.is_none());
        assert!(c.info(b).drop.is_some());
        assert_eq!(c.info(b).layout, Layout::new::<String>());
    }
}
