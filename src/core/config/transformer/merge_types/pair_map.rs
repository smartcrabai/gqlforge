use std::collections::HashMap;
use std::hash::Hash;

///
/// A special map that can hold two values of same type as key and any type of
/// value.
#[derive(Default)]
pub struct PairMap<A, V> {
    map: HashMap<(A, A), V>,
}

impl<A: PartialEq + Hash + Eq + Clone, V> PairMap<A, V> {
    pub fn add(&mut self, a1: A, a2: A, value: V) {
        self.map.insert((a1, a2), value);
    }

    pub fn get(&self, a1: &A, a2: &A) -> Option<&V> {
        self.map
            .get(&(a1.clone(), a2.clone()))
            .or_else(|| self.map.get(&(a2.clone(), a1.clone())))
    }
}
