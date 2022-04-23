use std::collections::BTreeSet;
use yzix_core::{StoreHash, WorkItem};

pub struct FullWorkItem {
    pub inhash: StoreHash,
    pub refs: BTreeSet<StoreHash>,
    pub inner: WorkItem,
}

impl FullWorkItem {
    pub fn new(inner: WorkItem, store_path: &camino::Utf8Path) -> Self {
        use yzix_core::Serialize as _;

        let mut hasher = StoreHash::get_hasher();
        inner.serialize(&mut hasher);
        let inhash = StoreHash::finalize_hasher(hasher);
        let stspec = crate::store_refs::build_store_spec(store_path);
        let mut e = crate::store_refs::Extract {
            spec: &stspec,
            refs: Default::default(),
        };
        yzix_core::visit_bytes::Element::accept(&inner, &mut e);
        Self {
            inhash,
            refs: e.refs,
            inner,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fwi_simple() {
        let a = yzix_core::WorkItem {
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
