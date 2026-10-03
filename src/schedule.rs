//! A schedule runs its systems in the order they were added, except that systems whose accesses
//! do not conflict may run at the same time on the shared pool. A system waits only for the
//! earlier systems it conflicts with. Commands are applied after every system has run, in order.

use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::mpsc;

use crate::system::{pool, systems_conflict, IntoSystem, System};
use crate::world::World;

#[derive(Default)]
pub struct Schedule {
    systems: Vec<Box<dyn System>>,
    initialized: usize,
    /// For each system, the earlier systems it must wait for.
    deps: Vec<Vec<usize>>,
    parallel: bool,
}

struct SendPtr<T: ?Sized>(*mut T);
unsafe impl<T: ?Sized> Send for SendPtr<T> {}
impl<T: ?Sized> Clone for SendPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: ?Sized> Copy for SendPtr<T> {}

impl Schedule {
    pub fn new() -> Schedule {
        Schedule { parallel: true, ..Default::default() }
    }

    /// Runs systems one after another on the calling thread (same order, same results).
    pub fn single_threaded(mut self) -> Schedule {
        self.parallel = false;
        self
    }

    pub fn add_system<M>(&mut self, s: impl IntoSystem<M>) -> &mut Self {
        self.systems.push(s.into_system());
        self
    }

    pub fn len(&self) -> usize {
        self.systems.len()
    }

    pub fn is_empty(&self) -> bool {
        self.systems.is_empty()
    }

    fn initialize(&mut self, world: &mut World) {
        if self.initialized == self.systems.len() {
            return;
        }
        for s in &mut self.systems[self.initialized..] {
            s.init(world);
        }
        self.initialized = self.systems.len();
        self.deps = (0..self.systems.len())
            .map(|j| (0..j).filter(|i| systems_conflict(self.systems[*i].as_ref(), self.systems[j].as_ref())).collect())
            .collect();
    }

    /// The batches a parallel run can achieve: systems grouped by their depth in the dependency
    /// graph. For display and tests; the executor starts each system as soon as it may.
    pub fn stages(&mut self, world: &mut World) -> Vec<Vec<String>> {
        self.initialize(world);
        let mut depth = vec![0usize; self.systems.len()];
        for j in 0..self.systems.len() {
            depth[j] = self.deps[j].iter().map(|i| depth[*i] + 1).max().unwrap_or(0);
        }
        let n = depth.iter().max().map(|d| d + 1).unwrap_or(0);
        let mut out = vec![Vec::new(); n];
        for (j, d) in depth.iter().enumerate() {
            out[*d].push(self.systems[j].name().to_string());
        }
        out
    }

    pub fn run(&mut self, world: &mut World) {
        self.initialize(world);
        let n = self.systems.len();
        if !self.parallel || n <= 1 || pool().threads() <= 1 {
            for s in &mut self.systems {
                if s.is_exclusive() {
                    s.run_exclusive(world);
                } else {
                    unsafe { s.run_unsafe(world) };
                }
            }
        } else {
            self.run_parallel(world);
        }
        for s in &mut self.systems {
            s.apply(world);
        }
    }

    fn run_parallel(&mut self, world: &mut World) {
        let n = self.systems.len();
        let mut waiting: Vec<usize> = self.deps.iter().map(|d| d.len()).collect();
        let mut dependents = vec![Vec::new(); n];
        for (j, d) in self.deps.iter().enumerate() {
            for i in d {
                dependents[*i].push(j);
            }
        }
        let world_ptr = SendPtr(world as *mut World);
        let systems: Vec<SendPtr<dyn System>> = self.systems.iter_mut().map(|s| SendPtr(s.as_mut() as *mut dyn System)).collect();
        let mut panic = None;
        pool().handle().scope(|scope| {
            let (tx, rx) = mpsc::channel::<(usize, Option<Box<dyn std::any::Any + Send>>)>();
            let mut ready: Vec<usize> = (0..n).filter(|j| waiting[*j] == 0).collect();
            let (mut running, mut done) = (0usize, 0usize);
            while done < n {
                // Exclusive systems conflict with everything, so when one is ready nothing else runs.
                while let Some(pos) = ready.iter().position(|j| unsafe { (*systems[*j].0).is_exclusive() }) {
                    if running > 0 {
                        break;
                    }
                    let j = ready.remove(pos);
                    let sys = systems[j].0;
                    let r = catch_unwind(AssertUnwindSafe(|| unsafe { (*sys).run_exclusive(&mut *world_ptr.0) }));
                    if let Err(p) = r {
                        panic.get_or_insert(p);
                    }
                    done += 1;
                    for d in &dependents[j] {
                        waiting[*d] -= 1;
                        if waiting[*d] == 0 {
                            ready.push(*d);
                        }
                    }
                }
                let mut i = 0;
                while i < ready.len() {
                    let j = ready[i];
                    if unsafe { (*systems[j].0).is_exclusive() } {
                        i += 1;
                        continue;
                    }
                    ready.remove(i);
                    running += 1;
                    let (tx, sys, w) = (tx.clone(), systems[j], world_ptr);
                    scope.spawn(move || {
                        let (sys, w) = (sys, w);
                        let r = catch_unwind(AssertUnwindSafe(|| unsafe { (*sys.0).run_unsafe(&*w.0) }));
                        let _ = tx.send((j, r.err()));
                    });
                }
                if done == n {
                    break;
                }
                if running == 0 {
                    continue;
                }
                let (j, p) = rx.recv().expect("system task lost");
                if let Some(p) = p {
                    panic.get_or_insert(p);
                }
                running -= 1;
                done += 1;
                for d in &dependents[j] {
                    waiting[*d] -= 1;
                    if waiting[*d] == 0 {
                        ready.push(*d);
                    }
                }
            }
        });
        if let Some(p) = panic {
            resume_unwind(p);
        }
    }
}
