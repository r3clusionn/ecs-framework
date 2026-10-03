//! Bundles: tuples of components spawned or inserted together, `(Position, Velocity)`.

use std::mem::ManuallyDrop;

use crate::component::{Component, ComponentId, Components};

/// A set of components. Implemented for tuples of up to 12 component types.
///
/// # Safety
/// `take` must hand over exactly the components `ids` lists, in the same order, each pointer
/// valid for one read, and must not drop them.
pub unsafe trait Bundle: Send + 'static {
    fn ids(components: &mut Components) -> Vec<ComponentId>;
    /// Moves every component out through `f(pointer)`, in the order of `ids`; `f` takes
    /// ownership of the bytes.
    fn take(self, f: &mut dyn FnMut(*const u8));
}

macro_rules! bundle {
    ($($t:ident $v:ident),*) => {
        unsafe impl<$($t: Component),*> Bundle for ($($t,)*) {
            #[allow(unused_variables)]
            fn ids(components: &mut Components) -> Vec<ComponentId> {
                vec![$(components.id::<$t>()),*]
            }
            #[allow(unused_variables)]
            fn take(self, f: &mut dyn FnMut(*const u8)) {
                let ($($v,)*) = self;
                $(
                    let $v = ManuallyDrop::new($v);
                    f(&*$v as *const $t as *const u8);
                )*
            }
        }
    };
}

bundle!();
bundle!(A a);
bundle!(A a, B b);
bundle!(A a, B b, C c);
bundle!(A a, B b, C c, D d);
bundle!(A a, B b, C c, D d, E e);
bundle!(A a, B b, C c, D d, E e, F f);
bundle!(A a, B b, C c, D d, E e, F f, G g);
bundle!(A a, B b, C c, D d, E e, F f, G g, H h);
bundle!(A a, B b, C c, D d, E e, F f, G g, H h, I i);
bundle!(A a, B b, C c, D d, E e, F f, G g, H h, I i, J j);
bundle!(A a, B b, C c, D d, E e, F f, G g, H h, I i, J j, K k);
bundle!(A a, B b, C c, D d, E e, F f, G g, H h, I i, J j, K k, L l);
