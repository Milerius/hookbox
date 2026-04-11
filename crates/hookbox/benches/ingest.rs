//! Benchmark for the ingest pipeline throughput.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use async_trait::async_trait;
use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use http::HeaderMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::emitter::ChannelEmitter;
use hookbox::error::StorageError;
use hookbox::pipeline::HookboxPipeline;
use hookbox::state::{ProcessingState, StoreResult};
use hookbox::traits::Storage;
use hookbox::types::{ReceiptFilter, WebhookReceipt};
use uuid::Uuid;

/// Minimal storage that just counts stores — no real persistence.
struct BenchStorage {
    count: Mutex<u64>,
}

impl BenchStorage {
    fn new() -> Self {
        Self {
            count: Mutex::new(0),
        }
    }
}

#[async_trait]
impl Storage for BenchStorage {
    async fn store(&self, _receipt: &WebhookReceipt) -> Result<StoreResult, StorageError> {
        let mut count = self
            .count
            .lock()
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        *count += 1;
        Ok(StoreResult::Stored)
    }

    async fn get(&self, _id: Uuid) -> Result<Option<WebhookReceipt>, StorageError> {
        Ok(None)
    }

    async fn update_state(
        &self,
        _id: Uuid,
        _state: ProcessingState,
        _error: Option<&str>,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn query(&self, _filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError> {
        Ok(vec![])
    }
}

fn bench_ingest(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    let mut group = c.benchmark_group("ingest");

    for size in [64, 256, 1024, 4096] {
        group.bench_with_input(BenchmarkId::new("body_size", size), &size, |b, &size| {
            let (emitter, mut rx) = ChannelEmitter::new(65536);
            rt.spawn(async move { while rx.recv().await.is_some() {} });

            let pipeline = HookboxPipeline::builder()
                .storage(BenchStorage::new())
                .dedupe(InMemoryRecentDedupe::new(100_000))
                .emitter(emitter)
                .build();

            let base_body = vec![b'x'; size];
            let counter = AtomicU64::new(0);

            b.to_async(&rt).iter(|| {
                let n = counter.fetch_add(1, Ordering::Relaxed);
                let body = Bytes::from(format!("{}{}", n, String::from_utf8_lossy(&base_body)));
                let headers = HeaderMap::new();
                async {
                    let _ = pipeline.ingest("bench", headers, body).await;
                }
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_ingest);
criterion_main!(benches);
