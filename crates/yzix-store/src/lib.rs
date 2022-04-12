use base64::engine::fast_portable::{FastPortable, FastPortableConfig};
pub use blake2::Digest;
use blake2::{digest::consts::U32, Blake2b};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::{convert, fmt};

mod dump;
// TODO: maybe rename Flags to something better...
pub use dump::{Dump, Flags};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
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
    pub fn hash_complex<T: serde::Serialize>(x: &T) -> Self {
        let mut ser = Vec::new();
        ciborium::ser::into_writer(x, &mut ser).unwrap();
        let mut hasher = Self::get_hasher();
        hasher.update(&ser[..]);
        Self::finalize_hasher(hasher)
    }
}

impl<'de> serde::Deserialize<'de> for Hash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use std::marker::PhantomData;
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

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, thiserror::Error)]
#[error("{real_path}: {kind}")]
pub struct Error {
    pub real_path: std::path::PathBuf,
    #[source]
    pub kind: ErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, thiserror::Error)]
pub enum ErrorKind {
    #[error("unable to convert symlink destination to UTF-8")]
    NonUtf8SymlinkTarget,

    #[error("unable to convert file name to UTF-8")]
    NonUtf8Basename,

    #[error("got unknown file type {0}")]
    UnknownFileType(String),

    #[error("directory entries with empty names are invalid")]
    EmptyBasename,

    #[error("symlinks are unsupported on this system")]
    #[cfg(not(any(unix, windows)))]
    SymlinksUnsupported,

    #[error("store dump declined to overwrite file")]
    /// NOTE: this obviously gets attached to the directory name
    OverwriteDeclined,

    #[error("I/O error: {desc}")]
    // NOTE: the `desc` already contains the os error code, don't print it 2 times.
    IoMisc { errno: Option<i32>, desc: String },
}

impl From<std::io::Error> for ErrorKind {
    #[inline]
    fn from(e: std::io::Error) -> ErrorKind {
        ErrorKind::IoMisc {
            errno: e.raw_os_error(),
            desc: e.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Hash;

    #[test]
    fn hash_parse() {
        assert_eq!(
            "m84OFxOfkVnnF7om15va9o1mgFcWD1TGH26ZhTLPuyg".parse(),
            Ok(Hash([
                155, 206, 14, 23, 19, 159, 145, 89, 231, 23, 186, 38, 215, 155, 218, 246, 141, 102,
                128, 87, 22, 15, 84, 198, 31, 110, 153, 133, 50, 207, 187, 40
            ]))
        );
    }

    #[test]
    fn hash_deser_from_str() {
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
        fn hash_roundtrip(s: [u8; 32]) {
            let x = Hash(s);
            proptest::prop_assert_eq!(Ok(x), x.to_string().parse());
        }
    }
}
