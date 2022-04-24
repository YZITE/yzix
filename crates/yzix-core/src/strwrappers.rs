use core::{convert, fmt};
use std::{borrow::Borrow, path::Path};

macro_rules! make_strwrapper {
    ($name:ident ( $inp:ident ) || $errmsg:expr; { $($x:tt)* }) => {
        #[derive(
            Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash,
            serde::Deserialize, serde::Serialize
        )]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            #[inline]
            pub fn new($inp: String) -> Option<Self> {
                $($x)*
            }
        }

        impl Borrow<String> for $name {
            #[inline(always)]
            fn borrow(&self) -> &String {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            #[inline(always)]
            fn borrow(&self) -> &str {
                &*self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = &'static str;
            #[inline]
            fn try_from(x: String) -> Result<Self, &'static str> {
                Self::new(x).ok_or($errmsg)
            }
        }

        impl From<$name> for String {
            #[inline(always)]
            fn from(x: $name) -> String {
                x.0
            }
        }

        impl convert::AsRef<str> for $name {
            #[inline(always)]
            fn as_ref(&self) -> &str {
                &*self.0
            }
        }

        impl convert::AsRef<[u8]> for $name {
            #[inline(always)]
            fn as_ref(&self) -> &[u8] {
                self.0.as_bytes()
            }
        }

        impl core::ops::Deref for $name {
            type Target = str;
            #[inline(always)]
            fn deref(&self) -> &str {
                &*self.0
            }
        }

        impl fmt::Display for $name {
            #[inline(always)]
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&*self.0)
            }
        }

        impl crate::Serialize for $name {
            #[inline(always)]
            fn serialize<U: crate::SerUpdate>(&self, state: &mut U) {
                self.0.serialize(state);
            }
        }
    }
}

make_strwrapper! { OutputName(outp) || "invalid output name"; {
    let is_illegal = |i: char| {
        !i.is_ascii_alphanumeric() && !matches!(i, '_' | '-' | '.')
    };
    if outp.is_empty() || outp.contains(is_illegal) {
        None
    } else {
        Some(Self(outp))
    }
}}

impl Default for OutputName {
    #[inline]
    fn default() -> Self {
        Self("out".into())
    }
}

pub fn is_default_output(o: &OutputName) -> bool {
    &*o.0 == "out"
}

make_strwrapper! { BaseName(efnam) || "invalid base name"; {
    let is_illegal = |i: char| {
        matches!(i, '\0' | '/') || std::path::is_separator(i)
    };
    match efnam.as_str() {
        "" | "." | ".." => None,
        _ if efnam.contains(is_illegal) => None,
        _ => Some(Self(efnam))
    }
}}

impl convert::AsRef<Path> for BaseName {
    #[inline(always)]
    fn as_ref(&self) -> &Path {
        Path::new(&*self.0)
    }
}

pub(crate) struct FilesDebug<'a>(pub(crate) &'a std::collections::BTreeMap<BaseName, crate::Dump>);

impl fmt::Debug for FilesDebug<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(
                self.0
                    .iter()
                    .map(|(k, v)| (&k.0, crate::StoreHash::hash_complex(v))),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{is_default_output, OutputName};

    #[test]
    fn default_output_valid() {
        let o = OutputName::default();
        assert!(is_default_output(&o));
        let _ = OutputName::new(o.0).expect("roundtrip failed");
    }
}
