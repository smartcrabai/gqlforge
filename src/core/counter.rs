use std::cell::Cell;

#[derive(Default)]
pub struct Counter<A>(Cell<A>);
impl<A> Counter<A> {
    pub fn new(start: A) -> Self {
        Self(Cell::new(start))
    }
}

impl<A: Copy + std::ops::Add<Output = A> + From<u8>> Counter<A> {
    pub fn next(&self) -> A {
        let curr = self.0.get();
        self.0.set(curr + A::from(1_u8));
        curr
    }
}
