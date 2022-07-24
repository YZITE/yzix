use camino::Utf8Path;
use lru::LruCache;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use store_ref_scanner::{StoreRefScanner, StoreSpec};
use yzix_core::{visit_bytes as yvb, TaggedHash, ThinTree};

pub struct Extract<'a, T> {
    pub spec: &'a StoreSpec<'a>,
    pub refs: BTreeSet<TaggedHash<T>>,
}

impl<T> yvb::Visitor for Extract<'_, T> {
    fn visit_bytes(&mut self, bytes: &[u8]) {
        self.refs.extend(
            StoreRefScanner::new(bytes, self.spec)
                // SAFETY: we know that only ASCII chars are possible here
                .map(|x| TaggedHash::from_str(std::str::from_utf8(x).unwrap()).unwrap()),
        );
    }
}

pub struct Rewrite<'a, T> {
    pub spec: &'a StoreSpec<'a>,
    pub rwtab: BTreeMap<TaggedHash<T>, TaggedHash<T>>,
}

impl<'a, T> yvb::VisitorMut for &'a Rewrite<'a, T> {
    fn visit_bytes(&mut self, bytes: &mut [u8]) {
        for i in StoreRefScanner::new(bytes, self.spec) {
            // SAFETY: we know that only ASCII chars are possible here
            let oldhash = TaggedHash::from_str(std::str::from_utf8(i).unwrap()).unwrap();
            if let Some(newhash) = self.rwtab.get(&oldhash) {
                i.copy_from_slice(newhash.to_string().as_bytes());
            }
        }
    }
}

pub struct Contains<'a, T> {
    pub spec: &'a StoreSpec<'a>,
    pub refs: &'a BTreeSet<TaggedHash<T>>,
    pub cont: bool,
}

impl<T> yvb::Visitor for Contains<'_, T> {
    fn visit_bytes(&mut self, bytes: &[u8]) {
        for i in StoreRefScanner::new(bytes, self.spec) {
            // SAFETY: we know that only ASCII chars are possible here
            let oldhash = TaggedHash::from_str(std::str::from_utf8(i).unwrap()).unwrap();
            if self.refs.contains(&oldhash) {
                self.cont = true;
            }
        }
    }
}

pub fn build_store_spec(store_path: &Utf8Path) -> store_ref_scanner::StoreSpec<'_> {
    StoreSpec {
        path_to_store: store_path.as_str(),
        // necessary because we don't attach names to store entries
        // and we don't want to pull in characters after the name
        // (might happen when we parse a CBOR serialized message)
        // if the name is not terminated with a slash.
        valid_restbytes: Default::default(),
        ..StoreSpec::DFL_YZIX1
    }
}

pub type Cache = LruCache<TaggedHash<ThinTree>, BTreeSet<TaggedHash<ThinTree>>>;

pub fn determine_store_closure(
    store_path: &Utf8Path,
    cache: &mut Cache,
    refs: &mut BTreeSet<TaggedHash<ThinTree>>,
) {
    let stspec = build_store_spec(store_path);
    let mut new_refs = std::mem::take(refs);

    while !new_refs.is_empty() {
        for i in std::mem::take(&mut new_refs) {
            if !refs.insert(i) {
                // ref already crawled
                continue;
            }
            if let Some(tmp_refs) = cache.get(&i) {
                new_refs.extend(tmp_refs.iter().copied());
            } else {
                use yvb::Element;
                let mut e = Extract {
                    spec: &stspec,
                    refs: BTreeSet::new(),
                };
                if let Ok(dump) = ThinTree::read_from_path(
                    store_path.join(i.to_string()).as_std_path(),
                    &mut |_, regu| {
                        // this prevents an allocation of memory for
                        // all files in a directory tree,
                        // instead handles files one-by-one.
                        regu.accept(&mut e);
                        Ok(())
                    },
                ) {
                    dump.accept(&mut e);
                    new_refs.extend(e.refs.iter().copied());
                    cache.put(i, e.refs);
                }
            }
        }
    }
}
