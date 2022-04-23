use camino::Utf8Path;
use lru::LruCache;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use store_ref_scanner::{StoreRefScanner, StoreSpec};
use yzix_store::{visit_bytes as yvb, Dump, Hash as StoreHash};

pub struct Extract<'a> {
    pub spec: &'a StoreSpec<'a>,
    pub refs: BTreeSet<StoreHash>,
}

impl yvb::Visitor for Extract<'_> {
    fn visit_bytes(&mut self, bytes: &[u8]) {
        self.refs.extend(
            StoreRefScanner::new(bytes, self.spec)
                // SAFETY: we know that only ASCII chars are possible here
                .map(|x| StoreHash::from_str(std::str::from_utf8(x).unwrap()).unwrap()),
        );
    }
}

pub struct Rewrite<'a> {
    pub spec: &'a StoreSpec<'a>,
    pub rwtab: BTreeMap<StoreHash, StoreHash>,
}

impl<'a> yvb::VisitorMut for &'a Rewrite<'a> {
    fn visit_bytes(&mut self, bytes: &mut [u8]) {
        for i in StoreRefScanner::new(bytes, self.spec) {
            // SAFETY: we know that only ASCII chars are possible here
            let oldhash = StoreHash::from_str(std::str::from_utf8(i).unwrap()).unwrap();
            if let Some(newhash) = self.rwtab.get(&oldhash) {
                i.copy_from_slice(newhash.to_string().as_bytes());
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

pub type Cache = LruCache<StoreHash, BTreeSet<StoreHash>>;

pub fn determine_store_closure(
    store_path: &Utf8Path,
    cache: &mut Cache,
    refs: &mut BTreeSet<StoreHash>,
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
            } else if let Ok(dump) =
                Dump::read_from_path(store_path.join(i.to_string()).as_std_path())
            {
                let mut e = Extract {
                    spec: &stspec,
                    refs: BTreeSet::new(),
                };
                yvb::Element::accept(&dump, &mut e);
                new_refs.extend(e.refs.iter().copied());
                cache.put(i, e.refs);
            }
        }
    }
}
