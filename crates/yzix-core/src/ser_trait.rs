pub use digest::Update;

/// Serialization trait, designed to be portable comparable
/// (in contrast to std::hash::Hash)
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

mod padding {
    use core::{cmp, ops};

    pub const PAD_LEN: u8 = 8;
    pub type Length = u64;

    #[inline(always)]
    pub fn padding_len<T>(len: T) -> u8
    where
        T: Copy + From<u8> + cmp::PartialOrd + ops::Rem<Output = T> + ops::Sub<Output = T>,
        u8: TryFrom<T>,
    {
        let pad_len = T::from(PAD_LEN);
        let remainder = len % pad_len;
        if remainder > 0.into() {
            match (pad_len - remainder).try_into() {
                Ok(x) => x,
                Err(_) => unreachable!(),
            }
        } else {
            0
        }
    }

    #[inline]
    pub fn padding(len: usize) -> &'static [u8] {
        const PADDING: [u8; PAD_LEN as usize] = [0u8; PAD_LEN as usize];
        &PADDING[..padding_len(len).into()]
    }
}

#[cfg(test)]
mod tests {
    use super::Serialize as _;

    #[derive(Debug, Default)]
    struct VecWrap(Vec<u8>);

    impl digest::Update for VecWrap {
        fn update(&mut self, data: &[u8]) {
            self.0.extend_from_slice(data);
        }
    }

    #[test]
    fn x0() {
        let mut vw = VecWrap::default();
        "".serialize(&mut vw);
        assert_eq!([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,], vw.0[..]);
    }

    #[test]
    fn x1() {
        let mut vw = VecWrap::default();
        "h".serialize(&mut vw);
        assert_eq!(
            [
                0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, b'h', 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00,
            ],
            vw.0[..]
        );
    }

    #[test]
    fn x5() {
        let mut vw = VecWrap::default();
        "hewlo".serialize(&mut vw);
        assert_eq!(
            [
                0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, b'h', b'e', b'w', b'l', b'o', 0x00,
                0x00, 0x00,
            ],
            vw.0[..]
        );
    }

    #[test]
    fn x8() {
        let mut vw = VecWrap::default();
        "abcdefgh".serialize(&mut vw);
        assert_eq!(
            [
                0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, b'a', b'b', b'c', b'd', b'e', b'f',
                b'g', b'h',
            ],
            vw.0[..]
        );
    }

    proptest::proptest! {
        #[test]
        fn always_padded(v: Vec<u8>) {
            let mut vw = VecWrap::default();
            (&*v).serialize(&mut vw);
            assert_eq!(vw.0.len() % 8, 0);
        }
    }
}
