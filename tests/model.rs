//! Random operations on a world and on a plain model (a map from entity to optional components),
//! compared after every step: every entity's components, and the results of several queries.
//! One component owns heap data and counts its drops, so leaks and double drops show up.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};

use tessera::{Entity, With, Without, World};

static LIVE: AtomicI64 = AtomicI64::new(0);

#[derive(Debug, Clone, PartialEq)]
struct A(u32);
#[derive(Debug, Clone, PartialEq)]
struct B(u64);
#[derive(Debug, PartialEq)]
struct S(String);
#[derive(Debug, Clone, PartialEq)]
struct Z; // zero-sized
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(align(32))]
struct Wide(u8);

impl S {
    fn new(s: String) -> S {
        LIVE.fetch_add(1, Ordering::SeqCst);
        S(s)
    }
}
impl Drop for S {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Default, Debug, Clone, PartialEq)]
struct Model {
    a: Option<u32>,
    b: Option<u64>,
    s: Option<String>,
    z: bool,
    wide: Option<u8>,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn check(w: &mut World, m: &HashMap<Entity, Model>) {
    assert_eq!(w.len(), m.len());
    for (e, want) in m {
        let got = Model {
            a: w.get::<A>(*e).map(|x| x.0),
            b: w.get::<B>(*e).map(|x| x.0),
            s: w.get::<S>(*e).map(|x| x.0.clone()),
            z: w.has::<Z>(*e),
            wide: w.get::<Wide>(*e).map(|x| x.0),
        };
        assert_eq!(&got, want, "entity {e:?}");
    }
    let mut q = w.query_filtered::<(Entity, &A, Option<&S>), Without<B>>();
    let mut got: Vec<(Entity, u32, Option<String>)> = q.iter(w).map(|(e, a, s)| (e, a.0, s.map(|s| s.0.clone()))).collect();
    got.sort();
    let mut want: Vec<(Entity, u32, Option<String>)> =
        m.iter().filter(|(_, x)| x.a.is_some() && x.b.is_none()).map(|(e, x)| (*e, x.a.unwrap(), x.s.clone())).collect();
    want.sort();
    assert_eq!(got, want);
    let mut q = w.query_filtered::<Entity, (With<Z>, With<Wide>)>();
    let n = q.iter(w).count();
    assert_eq!(n, m.values().filter(|x| x.z && x.wide.is_some()).count());
    let mut q = w.query::<&Wide>();
    for v in q.iter(w) {
        assert_eq!(v as *const Wide as usize % 32, 0, "alignment");
    }
}

#[test]
fn random_operations_match_a_plain_model() {
    // Miri runs this a few thousand times slower; a smaller run still covers every operation.
    let (seeds, steps) = if cfg!(miri) { (2u64, 200) } else { (20, 600) };
    for seed in 1..=seeds {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let mut w = World::new();
        let mut m: HashMap<Entity, Model> = HashMap::new();
        let mut dead: Vec<Entity> = Vec::new();
        for step in 0..steps {
            let pick = |rng: &mut Rng, m: &HashMap<Entity, Model>| -> Option<Entity> {
                if m.is_empty() {
                    return None;
                }
                let mut keys: Vec<Entity> = m.keys().copied().collect();
                keys.sort();
                Some(keys[rng.below(keys.len() as u64) as usize])
            };
            match rng.below(10) {
                0..=2 => {
                    let v = rng.below(1000) as u32;
                    let e = match rng.below(4) {
                        0 => w.spawn((A(v),)),
                        1 => w.spawn((A(v), B(v as u64 * 3))),
                        2 => w.spawn((S::new(format!("s{v}")), Z)),
                        _ => w.spawn((A(v), S::new(format!("s{v}")), Wide(v as u8), Z)),
                    };
                    let mut model = Model::default();
                    if w.has::<A>(e) {
                        model.a = Some(v);
                    }
                    if w.has::<B>(e) {
                        model.b = Some(v as u64 * 3);
                    }
                    if w.has::<S>(e) {
                        model.s = Some(format!("s{v}"));
                    }
                    model.z = w.has::<Z>(e);
                    if w.has::<Wide>(e) {
                        model.wide = Some(v as u8);
                    }
                    m.insert(e, model);
                }
                3 => {
                    if let Some(e) = pick(&mut rng, &m) {
                        assert!(w.despawn(e));
                        m.remove(&e);
                        dead.push(e);
                    }
                }
                4 => {
                    if let Some(e) = pick(&mut rng, &m) {
                        let v = rng.below(1000) as u32;
                        w.insert(e, (A(v), S::new(format!("i{v}"))));
                        let x = m.get_mut(&e).unwrap();
                        x.a = Some(v);
                        x.s = Some(format!("i{v}"));
                    }
                }
                5 => {
                    if let Some(e) = pick(&mut rng, &m) {
                        let x = m.get_mut(&e).unwrap();
                        assert_eq!(w.remove::<S>(e).map(|s| s.0.clone()), x.s.take());
                    }
                }
                6 => {
                    if let Some(e) = pick(&mut rng, &m) {
                        let x = m.get_mut(&e).unwrap();
                        assert_eq!(w.remove::<A>(e).map(|a| a.0), x.a.take());
                        assert_eq!(w.remove::<Z>(e).is_some(), std::mem::take(&mut x.z));
                    }
                }
                7 => {
                    if let Some(e) = pick(&mut rng, &m) {
                        let b = rng.below(1 << 40);
                        w.insert(e, (B(b), Wide(7)));
                        let x = m.get_mut(&e).unwrap();
                        x.b = Some(b);
                        x.wide = Some(7);
                    }
                }
                8 => {
                    // Mutate through a query.
                    let mut q = w.query::<&mut A>();
                    for mut a in q.iter_mut(&mut w) {
                        a.0 += 1;
                    }
                    for x in m.values_mut() {
                        if let Some(a) = x.a.as_mut() {
                            *a += 1;
                        }
                    }
                }
                _ => {
                    // Stale ids are refused everywhere.
                    if let Some(e) = dead.last().copied() {
                        assert!(!w.contains(e) && w.get::<A>(e).is_none() && !w.despawn(e) && !w.insert(e, (A(0),)));
                    }
                }
            }
            if step % 7 == 0 {
                check(&mut w, &m);
            }
        }
        check(&mut w, &m);
        let live_strings = m.values().filter(|x| x.s.is_some()).count() as i64;
        assert_eq!(LIVE.load(Ordering::SeqCst), live_strings, "seed {seed}: owned components alive");
        drop(w);
        assert_eq!(LIVE.load(Ordering::SeqCst), 0, "seed {seed}: everything dropped exactly once");
    }
}
