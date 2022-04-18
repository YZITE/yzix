#![forbid(
    clippy::as_conversions,
    clippy::cast_ptr_alignment,
    clippy::let_underscore_drop,
    trivial_casts,
    unconditional_recursion,
    unsafe_code
)]

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

pub struct PoolGuard<T> {
    push: Sender<T>,
    value: Option<T>,
}

impl<T> std::ops::Deref for PoolGuard<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        self.value.as_ref().unwrap()
    }
}

impl<T> Drop for PoolGuard<T> {
    fn drop(&mut self) {
        let value = self.value.take().unwrap();
        self.push.try_send(value).unwrap();
    }
}

impl<T: Send> Pool<T> {
    pub async fn get(&self) -> PoolGuard<T> {
        PoolGuard {
            push: self.push.clone(),
            value: Some(self.pop.recv().await.expect("unable to receive from pool")),
        }
    }

    pub async fn push(&self, value: T) {
        self.push.send(value).await.expect("unable to send to pool");
    }
}
