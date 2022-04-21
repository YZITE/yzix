#![no_std]

mod padding;

pub use digest::Update;

pub trait Serialize {
    fn serialize<U: Update>(&self, state: &mut U);
}

impl Serialize for usize {
    #[inline]
    fn serialize<U: Update>(&self, state: &mut U) {
        let len = padding::Length::try_from(*self).unwrap();
        state.update(&len.to_le_bytes());
    }
}

impl Serialize for [u8] {
    #[inline]
    fn serialize<U: Update>(&self, state: &mut U) {
        self.len().serialize(state);
        state.update(self);
        state.update(padding::padding(self.len()));
    }
}

impl Serialize for str {
    #[inline(always)]
    fn serialize<U: Update>(&self, state: &mut U) {
        self.as_bytes().serialize(state);
    }
}
