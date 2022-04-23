pub trait Element {
    fn accept<V: Visitor>(&self, visitor: &mut V);
    fn accept_mut<V: VisitorMut>(&mut self, visitor: &mut V);
}

pub trait Visitor {
    fn visit_bytes(&mut self, bytes: &[u8]);
}

pub trait VisitorMut {
    fn visit_bytes(&mut self, bytes: &mut [u8]);
}

impl Element for [u8] {
    fn accept<V: Visitor>(&self, visitor: &mut V) {
        visitor.visit_bytes(self);
    }
    fn accept_mut<V: VisitorMut>(&mut self, visitor: &mut V) {
        visitor.visit_bytes(self);
    }
}

impl Element for String {
    fn accept<V: Visitor>(&self, visitor: &mut V) {
        visitor.visit_bytes(self.as_bytes());
    }
    fn accept_mut<V: VisitorMut>(&mut self, visitor: &mut V) {
        let mut bytes = self.as_bytes().to_vec();
        visitor.visit_bytes(&mut bytes[..]);
        *self = String::from_utf8(bytes).expect("illegal hash characters used");
    }
}
