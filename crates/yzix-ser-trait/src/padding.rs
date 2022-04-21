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
