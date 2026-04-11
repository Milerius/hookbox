//! Property tests for emitter traits and `NormalizedEvent` serialization.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use hookbox::emitter::ChannelEmitter;
    use hookbox::state::ReceiptId;
    use hookbox::traits::Emitter;
    use hookbox::types::NormalizedEvent;

    fn make_event(provider: &str, hash: &str) -> NormalizedEvent {
        NormalizedEvent {
            receipt_id: ReceiptId::new(),
            provider_name: provider.to_owned(),
            event_type: Some("test.event".to_owned()),
            external_reference: None,
            parsed_payload: Some(serde_json::json!({"key": "value"})),
            payload_hash: hash.to_owned(),
            received_at: Utc::now(),
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn normalized_event_always_serializes_to_json() {
        bolero::check!()
            .with_type::<(String, String)>()
            .for_each(|(provider, hash)| {
                let event = make_event(provider, hash);
                let json = serde_json::to_string(&event).expect("serialize must succeed");
                let round_tripped: NormalizedEvent =
                    serde_json::from_str(&json).expect("deserialize must succeed");
                assert_eq!(event.provider_name, round_tripped.provider_name);
                assert_eq!(event.payload_hash, round_tripped.payload_hash);
                assert_eq!(event.receipt_id, round_tripped.receipt_id);
            });
    }

    #[test]
    fn arc_emitter_roundtrip() {
        bolero::check!().with_type::<String>().for_each(|provider| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime must build");
            rt.block_on(async {
                let (emitter, mut rx) = ChannelEmitter::new(16);
                let arc_emitter: Arc<dyn Emitter + Send + Sync> = Arc::new(emitter);
                let event = make_event(provider, "abc123");
                arc_emitter.emit(&event).await.expect("emit must succeed");
                let received = rx.try_recv().expect("must receive event");
                assert_eq!(received.provider_name, event.provider_name);
            });
        });
    }

    #[test]
    fn receipt_id_is_valid_message_key() {
        bolero::check!().for_each(|_: &[u8]| {
            let id = ReceiptId::new();
            let s = id.to_string();
            assert!(!s.is_empty(), "receipt_id must not be empty");
            assert!(!s.contains('\n'), "receipt_id must not contain newlines");
            assert!(!s.contains('\0'), "receipt_id must not contain nulls");
        });
    }
}
