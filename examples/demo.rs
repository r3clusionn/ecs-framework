//! A small simulation: 100,000 particles fall, bounce and age; old ones are despawned and new
//! ones spawned by commands. Prints how the schedule groups the systems and how long frames take.
//!
//!     cargo run --release --example demo

use std::time::Instant;

use tessera::{Changed, Commands, Entity, Query, Res, ResMut, Schedule, With, Without, World};

#[derive(Clone, Copy)]
struct Pos(f32, f32);
#[derive(Clone, Copy)]
struct Vel(f32, f32);
#[derive(Clone, Copy)]
struct Age(u32);
struct Frozen;
struct Gravity(f32);
#[derive(Default)]
struct Stats {
    spawned: u64,
    despawned: u64,
    bounces: u64,
}

fn gravity(mut q: Query<&mut Vel, Without<Frozen>>, g: Res<Gravity>) {
    q.par_for_each_mut(4096, |mut v| v.1 -= g.0);
}

fn movement(mut q: Query<(&mut Pos, &Vel)>) {
    q.par_for_each_mut(4096, |(mut p, v)| {
        p.0 += v.0;
        p.1 += v.1;
    });
}

fn bounce(mut q: Query<(&mut Pos, &mut Vel), Changed<Pos>>, mut stats: ResMut<Stats>) {
    let mut n = 0;
    for (mut p, mut v) in q.iter_mut() {
        if p.1 < 0.0 {
            p.1 = -p.1;
            v.1 = -v.1 * 0.8;
            n += 1;
        }
    }
    stats.bounces += n;
}

fn ageing(mut q: Query<&mut Age>) {
    for mut a in q.iter_mut() {
        a.0 += 1;
    }
}

fn recycle(q: Query<(Entity, &Age), Without<Frozen>>, mut c: Commands, mut stats: ResMut<Stats>) {
    for (e, a) in q.iter() {
        if a.0 > 300 {
            c.despawn(e);
            c.spawn((Pos(0.0, 100.0), Vel(1.0, 0.0), Age(0)));
            stats.despawned += 1;
            stats.spawned += 1;
        }
    }
}

fn frozen_count(q: Query<(), With<Frozen>>) {
    assert!(q.iter().count() > 0);
}

fn main() {
    let mut w = World::new();
    w.insert_resource(Gravity(0.1));
    w.insert_resource(Stats::default());
    w.spawn_batch((0..100_000).map(|i| (Pos(i as f32 * 0.01, 50.0 + (i % 50) as f32), Vel(0.0, 0.0), Age(i % 300))));
    w.spawn_batch((0..1_000).map(|i| (Pos(i as f32, 0.0), Vel(0.0, 0.0), Frozen)));
    let mut s = Schedule::new();
    s.add_system(gravity).add_system(movement).add_system(bounce).add_system(ageing).add_system(recycle).add_system(frozen_count);
    println!("{} entities, {} archetypes", w.len(), w.archetype_count());
    println!("stages (systems in one stage may run at the same time):");
    for (i, st) in s.stages(&mut w).iter().enumerate() {
        let names: Vec<&str> = st.iter().map(|n| n.rsplit("::").next().unwrap_or(n)).collect();
        println!("  {}: {}", i + 1, names.join(", "));
    }
    let frames = 600;
    let t = Instant::now();
    for _ in 0..frames {
        s.run(&mut w);
    }
    let el = t.elapsed();
    let st = w.resource::<Stats>().unwrap();
    println!(
        "{frames} frames in {:.1} ms ({:.0} us per frame, {} threads)",
        el.as_secs_f64() * 1e3,
        el.as_secs_f64() * 1e6 / frames as f64,
        tessera::system::pool().threads()
    );
    println!("bounces {}, despawned {}, spawned {}, entities now {}", st.bounces, st.despawned, st.spawned, w.len());
}
