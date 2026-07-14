use std::sync::{Arc, RwLock};

use sqlx::SqlitePool;
use tokio::sync::Mutex;

use crate::{
    config::AppConfig,
    storage::{
        settings::{self, RuntimeSettings},
        upstreams::{self, Upstream},
    },
};

#[derive(Clone)]
pub struct RuntimeSnapshot {
    pub settings: RuntimeSettings,
    pub upstreams: Vec<Upstream>,
}

impl RuntimeSnapshot {
    pub async fn load(pool: &SqlitePool, config: &AppConfig) -> Result<Self, sqlx::Error> {
        Ok(Self {
            settings: settings::get_runtime_settings(pool, config).await?,
            upstreams: upstreams::list_runtime(pool).await?,
        })
    }

    pub fn configured_upstreams(&self) -> Vec<Upstream> {
        self.upstreams.clone()
    }
}

#[derive(Clone)]
pub struct RuntimeCache {
    inner: Arc<RwLock<RuntimeSnapshot>>,
    refresh_lock: Arc<Mutex<()>>,
}

impl RuntimeCache {
    pub async fn load(pool: &SqlitePool, config: &AppConfig) -> Result<Self, sqlx::Error> {
        Ok(Self {
            inner: Arc::new(RwLock::new(RuntimeSnapshot::load(pool, config).await?)),
            refresh_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        self.inner
            .read()
            .expect("runtime cache lock should not be poisoned")
            .clone()
    }

    pub async fn refresh(&self, pool: &SqlitePool, config: &AppConfig) -> Result<(), sqlx::Error> {
        let _guard = self.refresh_lock.lock().await;
        let next = RuntimeSnapshot::load(pool, config).await?;
        *self
            .inner
            .write()
            .expect("runtime cache lock should not be poisoned") = next;
        Ok(())
    }
}
