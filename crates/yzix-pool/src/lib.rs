use async_channel::{unbounded, Receiver, Sender};

#[derive(Clone)]
pub struct Pool<T> {
    push: Sender<T>,
    pop: Receiver<T>,
}

impl<T> Default for Pool<T> {
    #[inline]
    fn default() -> Self {
        let (push, pop) = unbounded();
        Self { push, pop }
    }
}

impl<T: Send> Pool<T> {
    #[inline]
    pub async fn pop(&self) -> T {
        self.pop
            .recv()
            .await
            .expect("unable to receive from pool")
    }

    #[inline]
    pub async fn push(&self, x: T) {
        self.push
            .send(x)
            .await
            .expect("unable to send to pool")
    }
}
