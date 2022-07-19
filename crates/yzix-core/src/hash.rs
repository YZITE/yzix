use base64::engine::fast_portable::{FastPortable, FastPortableConfig};
use blake2::{digest::consts::U32, Blake2b, Digest as _};
use core::{cmp, convert, fmt, marker::PhantomData};
use once_cell::sync::Lazy;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct Hash(pub [u8; 32]);

static B64_ALPHABET: Lazy<base64::alphabet::Alphabet> = Lazy::new(|| {
    // base64, like URL_SAFE, but '-' is replaced with '+' for better interaction
    // with shell programs, which don't like file names which start with '-'
    base64::alphabet::Alphabet::from_str(
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+_",
    )
    .unwrap()
});

static B64_ENGINE: Lazy<FastPortable> = Lazy::new(|| {
    FastPortable::from(
        &*B64_ALPHABET,
        FastPortableConfig::new().with_encode_padding(false),
    )
});

impl fmt::Display for Hash {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&base64::encode_engine(self.0, &*B64_ENGINE))
    }
}

impl fmt::Debug for Hash {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\"", self)
    }
}

impl std::str::FromStr for Hash {
    type Err = base64::DecodeError;
    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(
            base64::decode_engine(s, &*B64_ENGINE)?
                .try_into()
                .map_err(|_| base64::DecodeError::InvalidLength)?,
        ))
    }
}

impl convert::AsRef<[u8]> for Hash {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Hash {
    #[inline]
    pub fn get_hasher() -> Blake2b<U32> {
        Blake2b::new()
    }

    #[inline]
    pub fn finalize_hasher(x: Blake2b<U32>) -> Self {
        Self(x.finalize().try_into().unwrap())
    }

    /// NOTE: it is recommended to always specify the type paramter when calling
    /// this function to prevent accidential hash changes
    /// (e.g. forgot to deref or such)
    pub fn hash_complex<T: crate::Serialize>(x: &T) -> Self {
        let mut hasher = Self::get_hasher();
        x.serialize(&mut hasher);
        Self::finalize_hasher(hasher)
    }
}

impl<'de> serde::Deserialize<'de> for Hash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor<'de>(PhantomData<&'de ()>);
        impl<'de> serde::de::Visitor<'de> for Visitor<'de> {
            type Value = Hash;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                fmt::Formatter::write_str(f, "tuple struct Hash")
            }
            #[inline]
            fn visit_newtype_struct<E>(self, e: E) -> Result<Self::Value, E::Error>
            where
                E: serde::Deserializer<'de>,
            {
                serde::Deserialize::deserialize(e).map(Hash)
            }
            #[inline]
            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                serde::de::SeqAccess::next_element(&mut seq)?
                    .ok_or_else(|| {
                        serde::de::Error::invalid_length(
                            0usize,
                            &"tuple struct Hash with 1 element",
                        )
                    })
                    .map(Hash)
            }
            // additional method, not derived normally
            #[inline]
            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                core::str::FromStr::from_str(s).map_err(serde::de::Error::custom)
            }
        }
        serde::Deserializer::deserialize_newtype_struct(deserializer, "Hash", Visitor(PhantomData))
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(transparent, bound = "")]
#[repr(transparent)]
pub struct TaggedHash<T> {
    inner: Hash,
    #[serde(default, skip_serializing)]
    _phantom: PhantomData<*const T>,
}

impl<T> convert::AsRef<Hash> for TaggedHash<T> {
    #[inline(always)]
    fn as_ref(&self) -> &Hash {
        &self.inner
    }
}

impl<T: crate::Serialize> TaggedHash<T> {
    #[inline]
    pub fn hash_complex(t: &T) -> Self {
        Self {
            inner: Hash::hash_complex::<T>(t),
            _phantom: PhantomData,
        }
    }
}

impl<T> core::clone::Clone for TaggedHash<T> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            inner: self.inner,
            _phantom: PhantomData,
        }
    }
    #[inline]
    fn clone_from(&mut self, source: &Self) {
        self.inner = source.inner;
    }
}

impl<T> fmt::Debug for TaggedHash<T> {
    #[inline(always)]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        <Hash as fmt::Debug>::fmt(&self.inner, f)
    }
}

impl<T> cmp::PartialEq for TaggedHash<T> {
    #[inline]
    fn eq(&self, oth: &Self) -> bool {
        self.inner == oth.inner
    }
}

impl<T> core::marker::Copy for TaggedHash<T> {}
impl<T> cmp::Eq for TaggedHash<T> {}

#[cfg(test)]
mod tests {
    use super::Hash;

    #[test]
    fn parse() {
        assert_eq!(
            "m84OFxOfkVnnF7om15va9o1mgFcWD1TGH26ZhTLPuyg".parse(),
            Ok(Hash([
                155, 206, 14, 23, 19, 159, 145, 89, 231, 23, 186, 38, 215, 155, 218, 246, 141, 102,
                128, 87, 22, 15, 84, 198, 31, 110, 153, 133, 50, 207, 187, 40
            ]))
        );
    }

    #[test]
    fn deser_from_str() {
        use serde_test::Token;
        serde_test::assert_de_tokens(
            &Hash([
                155, 206, 14, 23, 19, 159, 145, 89, 231, 23, 186, 38, 215, 155, 218, 246, 141, 102,
                128, 87, 22, 15, 84, 198, 31, 110, 153, 133, 50, 207, 187, 40,
            ]),
            &[Token::Str("m84OFxOfkVnnF7om15va9o1mgFcWD1TGH26ZhTLPuyg")],
        );
    }

    proptest::proptest! {
        #[test]
        fn roundtrip(s: [u8; 32]) {
            let x = Hash(s);
            proptest::prop_assert_eq!(Ok(x), x.to_string().parse());
        }
    }
}
