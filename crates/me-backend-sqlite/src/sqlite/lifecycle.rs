//! `impl Lifecycle for SqliteBackend` — a delivery and its effects in one transaction on the
//! write connection (#1025), over [`store::lifecycle`](crate::store::lifecycle).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::SqliteBackend;
use crate::store::lifecycle::{self, ReceiptKey};
use me_storage::lifecycle::Lifecycle;
use me_types::error::Result;
use me_types::types::lifecycle::{Applied, Batch, BatchMode, DeliveryWrite, Keyed};

#[async_trait]
impl Lifecycle for SqliteBackend {
    // WRITE
    async fn record_delivery(
        &self,
        machine: &str,
        event_id: &str,
        body_sha256: &[u8; 32],
        received_at: DateTime<Utc>,
        write: DeliveryWrite,
    ) -> Result<Applied> {
        let machine = machine.to_owned();
        let event_id = event_id.to_owned();
        let body_sha256 = *body_sha256;
        let registry = Arc::clone(&self.upcaster_registry);
        let dim = self.embed_dim;
        self.block_write(move |c| {
            let key = ReceiptKey {
                machine: &machine,
                event_id: &event_id,
                body_sha256: &body_sha256,
                received_at,
            };
            lifecycle::record_delivery(c, &registry, dim, &key, &write)
        })
        .await
    }

    // WRITE
    async fn record_deliveries(
        &self,
        machine: &str,
        received_at: DateTime<Utc>,
        items: Vec<Keyed>,
        context: [u8; 32],
        mode: BatchMode,
    ) -> Result<Batch> {
        let machine = machine.to_owned();
        let registry = Arc::clone(&self.upcaster_registry);
        let dim = self.embed_dim;
        self.block_write(move |c| {
            lifecycle::record_deliveries(
                c,
                &registry,
                dim,
                &machine,
                received_at,
                &items,
                &context,
                mode,
            )
        })
        .await
    }

    // WRITE
    async fn end_idle_segments(
        &self,
        idle_before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        self.block_write(move |c| lifecycle::end_idle_segments(c, idle_before, now))
            .await
    }
}
