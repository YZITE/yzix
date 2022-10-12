use blake2::{digest::consts::U32, Blake2b, Digest as _};
use core::{cmp, convert, fmt, marker::PhantomData};
use yzb64::{base64, B64_ENGINE};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash(pub [u8; 32]);

impl fmt::Display for Hash {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        base64::display::Base64Display::from(&self.0, &*B64_ENGINE).fmt(f)
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

impl serde::Serialize for Hash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if serializer.is_human_readable() {
            self.to_string().serialize(serializer)
        } else {
            serializer.serialize_bytes(&self.0)
        }
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
        if deserializer.is_human_readable() {
            <String as serde::Deserialize<'de>>::deserialize(deserializer)
                .and_then(|s| core::str::FromStr::from_str(&s).map_err(serde::de::Error::custom))
        } else {
            serde::Deserialize::deserialize(deserializer).map(Hash)
            //serde::Deserializer::deserialize_newtype_struct(deserializer, "Hash", Visitor(PhantomData))
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(transparent, bound = "")]
#[repr(transparent)]
pub struct TaggedHash<T> {
    inner: Hash,
    #[serde(default, skip_serializing)]
    _phantom: PhantomData<fn() -> T>,
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

    #[inline]
    pub fn unsafe_cast(inner: Hash) -> Self {
        Self {
            inner,
            _phantom: PhantomData,
        }
    }
}

impl<T> crate::Serialize for TaggedHash<T> {
    #[inline(always)]
    fn serialize<H: crate::SerUpdate>(&self, state: &mut H) {
        self.inner.0.serialize(state);
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
    #[inline(always)]
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

impl<T> fmt::Display for TaggedHash<T> {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        <Hash as fmt::Display>::fmt(&self.inner, f)
    }
}

impl<T> cmp::PartialEq for TaggedHash<T> {
    #[inline]
    fn eq(&self, oth: &Self) -> bool {
        self.inner == oth.inner
    }
}

impl<T> cmp::PartialOrd for TaggedHash<T> {
    #[inline]
    fn partial_cmp(&self, oth: &Self) -> Option<cmp::Ordering> {
        Some(self.inner.cmp(&oth.inner))
    }
}

impl<T> cmp::Ord for TaggedHash<T> {
    #[inline]
    fn cmp(&self, oth: &Self) -> cmp::Ordering {
        self.inner.cmp(&oth.inner)
    }
}

impl<T> core::hash::Hash for TaggedHash<T> {
    #[inline(always)]
    fn hash<H: core::hash::Hasher>(&self, h: &mut H) {
        self.inner.hash(h);
    }
}

impl<T> core::marker::Copy for TaggedHash<T> {}
impl<T> cmp::Eq for TaggedHash<T> {}

impl<T> std::str::FromStr for TaggedHash<T> {
    type Err = base64::DecodeError;

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<Hash>().map(|inner| Self {
            inner,
            _phantom: PhantomData,
        })
    }
}

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
        use serde_test::{Configure, Token};
        serde_test::assert_de_tokens(
            &Hash([
                155, 206, 14, 23, 19, 159, 145, 89, 231, 23, 186, 38, 215, 155, 218, 246, 141, 102,
                128, 87, 22, 15, 84, 198, 31, 110, 153, 133, 50, 207, 187, 40,
            ])
            .readable(),
            &[Token::Str("m84OFxOfkVnnF7om15va9o1mgFcWD1TGH26ZhTLPuyg")],
        );
    }

    #[test]
    fn deser_from_json_str() {
        assert_eq!(
            serde_json::from_str::<Hash>("\"m84OFxOfkVnnF7om15va9o1mgFcWD1TGH26ZhTLPuyg\"")
                .unwrap(),
            Hash([
                155, 206, 14, 23, 19, 159, 145, 89, 231, 23, 186, 38, 215, 155, 218, 246, 141, 102,
                128, 87, 22, 15, 84, 198, 31, 110, 153, 133, 50, 207, 187, 40,
            ])
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
