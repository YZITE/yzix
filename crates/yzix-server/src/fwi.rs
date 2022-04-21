use std::collections::BTreeSet;
use yzix_proto::store::Hash as StoreHash;

pub struct FullWorkItem {
    pub inhash: StoreHash,
    pub refs: BTreeSet<StoreHash>,
    pub inner: yzix_proto::WorkItem,
}

impl FullWorkItem {
    pub fn new(inner: yzix_proto::WorkItem, store_path: &camino::Utf8Path) -> Self {
        use std::str::FromStr;
        use store_ref_scanner as srs;
        use yzix_proto::store::Digest;

        // to make this more effective, serialize just once
        let mut ser = Vec::new();
        yzix_proto::ciborium::ser::into_writer(&inner, &mut ser).unwrap();
        let mut hasher = StoreHash::get_hasher();
        hasher.update(&ser[..]);
        let inhash = StoreHash::finalize_hasher(hasher);
        let refs =
            srs::StoreRefScanner::new(&ser[..], &yzix_store_refs::build_store_spec(store_path))
                // SAFETY: we know that only ASCII chars are possible here,
                // and `build_store_spec` ensures that the hash is correctly truncated
                .map(|x| StoreHash::from_str(std::str::from_utf8(x).unwrap()).unwrap())
                .collect();
        Self {
            inhash,
            refs,
            inner,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fwi_simple() {
        let a = yzix_proto::WorkItem {
            args: vec![
                "/yzix_s/4Zx1PBoft1YyAuKdhjAY1seZFHloxQ+8voHQRkRMuys/bin/whoops".to_string(),
            ],
            envs: Default::default(),
            outputs: Default::default(),
        };
        let fwi = FullWorkItem::new(a, "/yzix_s".into());
        assert_eq!(
            fwi.refs.into_iter().collect::<Vec<_>>(),
            vec!["4Zx1PBoft1YyAuKdhjAY1seZFHloxQ+8voHQRkRMuys"
                .parse::<StoreHash>()
                .unwrap()]
        );
    }
}
