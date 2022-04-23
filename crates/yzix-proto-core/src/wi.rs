use std::collections::{BTreeMap, BTreeSet};
use yzix_store as ys;
use yzix_visit_bytes as yvb;

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct WorkItem {
    pub args: Vec<String>,
    pub envs: BTreeMap<String, String>,
    pub outputs: BTreeSet<crate::OutputName>,
}

impl ys::Serialize for WorkItem {
    fn serialize<U: ys::Update>(&self, state: &mut U) {
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
        ")".serialize(state);
    }
}

impl yvb::Element for WorkItem {
    fn accept<V: yvb::Visitor>(&self, visitor: &mut V) {
        self.args.iter().for_each(|x| x.accept(visitor));
        self.envs.values().for_each(|x| x.accept(visitor));
    }
    fn accept_mut<V: yvb::VisitorMut>(&mut self, visitor: &mut V) {
        self.args.iter_mut().for_each(|x| x.accept_mut(visitor));
        self.envs.values_mut().for_each(|x| x.accept_mut(visitor));
    }
}
