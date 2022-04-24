use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct WorkItem {
    pub args: Vec<String>,
    pub envs: BTreeMap<String, String>,
    pub outputs: BTreeSet<crate::OutputName>,

    // used for e.g. structured attrs, where it doesn't make sense to first
    // serialize a dump to the store to use it only once
    // the files get placed into the `/build` directory
    pub files: BTreeMap<crate::BaseName, crate::Dump>,
}

impl fmt::Debug for WorkItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkItem")
            .field("args", &self.args)
            .field("envs", &self.envs)
            .field("outputs", &self.outputs)
            .field("files", &crate::strwrappers::FilesDebug(&self.files))
            .finish()
    }
}

impl crate::Serialize for WorkItem {
    fn serialize<U: crate::SerUpdate>(&self, state: &mut U) {
        "(".serialize(state);
        "workitem".serialize(state);
        "args".serialize(state);
        self.args.len().serialize(state);
        for arg in &self.args {
            arg.serialize(state);
        }
        "envs".serialize(state);
        self.envs.len().serialize(state);
        for (k, v) in &self.envs {
            "(".serialize(state);
            k.serialize(state);
            v.serialize(state);
            ")".serialize(state);
        }
        "outputs".serialize(state);
        self.outputs.len().serialize(state);
        for o in &self.outputs {
            o.serialize(state);
        }
        if !self.files.is_empty() {
            "files".serialize(state);
            self.files.len().serialize(state);
            for (fname, fdump) in &self.files {
                "(".serialize(state);
                fname.serialize(state);
                fdump.serialize(state);
                ")".serialize(state);
            }
        }
        ")".serialize(state);
    }
}

impl crate::visit_bytes::Element for WorkItem {
    fn accept<V: crate::visit_bytes::Visitor>(&self, visitor: &mut V) {
        self.args.iter().for_each(|x| x.accept(visitor));
        self.envs.values().for_each(|x| x.accept(visitor));
    }
    fn accept_mut<V: crate::visit_bytes::VisitorMut>(&mut self, visitor: &mut V) {
        self.args.iter_mut().for_each(|x| x.accept_mut(visitor));
        self.envs.values_mut().for_each(|x| x.accept_mut(visitor));
    }
}
