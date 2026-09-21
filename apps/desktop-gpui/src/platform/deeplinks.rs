use std::sync::{Arc, Mutex};

use desktop_runtime::{
    Result, ServiceError,
    deeplink::{DeepLink, DeepLinkInbox},
};
use gpui::Application;

#[derive(Clone, Default)]
pub struct NativeDeepLinks {
    inbox: Arc<Mutex<DeepLinkInbox>>,
}

impl NativeDeepLinks {
    pub fn install(&self, application: &Application, notify: impl Fn(Result<u64>) + 'static) {
        let links = self.clone();
        application.on_open_urls(move |urls| {
            for url in urls {
                notify(links.receive(&url));
            }
        });
    }

    pub fn receive(&self, raw: &str) -> Result<u64> {
        self.inbox
            .lock()
            .map_err(|_| ServiceError::Closed)?
            .push(raw)
    }

    pub fn pending(&self, shares: bool) -> Result<Vec<(u64, Arc<DeepLink>)>> {
        Ok(self
            .inbox
            .lock()
            .map_err(|_| ServiceError::Closed)?
            .pending(shares)
            .cloned()
            .collect())
    }

    pub fn acknowledge(&self, id: u64) -> Result<bool> {
        Ok(self
            .inbox
            .lock()
            .map_err(|_| ServiceError::Closed)?
            .acknowledge(id))
    }
}
