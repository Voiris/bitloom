use std::future::Future;

use tokio::sync::{OnceCell, watch};

use crate::SharedTaskResult;

pub(crate) struct ResultSlot<T> {
    value: OnceCell<SharedTaskResult<T>>,
    completed: watch::Sender<bool>,
}

impl<T> Default for ResultSlot<T> {
    fn default() -> Self {
        let (completed, _) = watch::channel(false);
        Self { value: OnceCell::new(), completed }
    }
}

impl<T> ResultSlot<T> {
    pub(crate) async fn wait(&self) -> SharedTaskResult<T> {
        let mut completed = self.completed.subscribe();
        loop {
            if let Some(result) = self.value.get() {
                return result.clone();
            }

            if completed.changed().await.is_err() {
                continue;
            }
        }
    }

    pub(crate) async fn initialize<F, Fut>(&self, initializer: F) -> SharedTaskResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = SharedTaskResult<T>>,
    {
        let result = self.value.get_or_init(initializer).await.clone();
        self.completed.send_replace(true);
        result
    }

    pub(crate) async fn finish(&self, result: SharedTaskResult<T>) -> SharedTaskResult<T> {
        let _ = self.value.set(result);
        self.completed.send_replace(true);
        self.value.get().expect("a result slot is initialized after set returns").clone()
    }

    pub(crate) fn get(&self) -> Option<SharedTaskResult<T>> {
        self.value.get().cloned()
    }
}
