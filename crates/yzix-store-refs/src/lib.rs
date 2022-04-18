use camino::Utf8Path;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use store_ref_scanner::{StoreRefScanner, StoreSpec};
use yzix_store::{Dump, Hash as StoreHash};

pub fn extract_from_vec<'a>(
    spec: &'a StoreSpec,
    dat: &'a [u8],
) -> impl Iterator<Item = StoreHash> + 'a {
    StoreRefScanner::new(dat, spec)
        // SAFETY: we know that only ASCII chars are possible here
        .map(|x| StoreHash::from_str(std::str::from_utf8(x).unwrap()).unwrap())
}

pub fn extract_from_dump(spec: &StoreSpec, dump: &Dump, refs: &mut BTreeSet<StoreHash>) {
    match dump {
        Dump::Regular { contents, .. } => {
            refs.extend(extract_from_vec(spec, contents));
        }
        Dump::SymLink { target } => {
            refs.extend(extract_from_vec(spec, target.as_str().as_bytes()));
        }
        Dump::Directory(dir) => {
            dir.values().for_each(|v| extract_from_dump(spec, v, refs));
        }
    }
}

pub type RwTab = BTreeMap<StoreHash, StoreHash>;

pub fn rewrite_in_vec(spec: &StoreSpec, rwtab: &RwTab, dat: &mut [u8]) {
    for i in StoreRefScanner::new(dat, spec) {
        // SAFETY: we know that only ASCII chars are possible here
        let oldhash = StoreHash::from_str(std::str::from_utf8(i).unwrap()).unwrap();
        if let Some(newhash) = rwtab.get(&oldhash) {
            i.copy_from_slice(newhash.to_string().as_bytes());
        }
    }
}

pub fn rewrite_in_dump(spec: &StoreSpec, rwtab: &RwTab, dump: &mut Dump) {
    match dump {
        Dump::Regular { contents, .. } => {
            rewrite_in_vec(spec, rwtab, contents);
        }
        Dump::SymLink { target } => {
            let mut tmp_target: Vec<u8> = target.as_str().bytes().collect();
            rewrite_in_vec(spec, rwtab, &mut tmp_target[..]);
            *target = String::from_utf8(tmp_target)
                .expect("illegal hash characters used")
                .into();
        }
        Dump::Directory(dir) => {
            dir.values_mut()
                .for_each(|v| rewrite_in_dump(spec, rwtab, v));
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

pub fn determine_store_closure(store_path: &Utf8Path, refs: &mut BTreeSet<StoreHash>) {
    let stspec = build_store_spec(store_path);
    let mut new_refs = std::mem::take(refs);

    while !new_refs.is_empty() {
        for i in std::mem::take(&mut new_refs) {
            refs.insert(i);
            if let Ok(dump) = Dump::read_from_path(store_path.join(i.to_string()).as_std_path()) {
                extract_from_dump(&stspec, &dump, &mut new_refs);
                new_refs.retain(|r| !refs.contains(r));
            }
        }
    }
}
