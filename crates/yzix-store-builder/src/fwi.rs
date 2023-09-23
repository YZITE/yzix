use std::collections::BTreeSet;
use yzix_core::{TaggedHash, ThinTree, WorkItem};

pub struct FullWorkItem {
    pub inhash: TaggedHash<WorkItem>,
    pub refs: BTreeSet<TaggedHash<ThinTree>>,
    pub inner: WorkItem,
}

impl FullWorkItem {
    pub fn new(inner: WorkItem, store_path: &camino::Utf8Path) -> Self {
        let inhash = TaggedHash::hash_complex(&inner);
        let stspec = crate::store_refs::build_store_spec(store_path);
        let mut e = crate::store_refs::Extract {
            spec: &stspec,
            refs: Default::default(),
        };
        visit_bytes::Element::accept(&inner, &mut e);
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
            ..Default::default()
        };
        let fwi = FullWorkItem::new(a, "/yzix_s".into());
        assert_eq!(
            fwi.refs.into_iter().collect::<Vec<_>>(),
            vec!["4Zx1PBoft1YyAuKdhjAY1seZFHloxQ+8voHQRkRMuys"
                .parse::<TaggedHash<_>>()
                .unwrap()]
        );
    }
}
