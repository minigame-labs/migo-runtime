//! One charged allocation survives encoding, header reveal, moves and refusal.
use crate::{
    ErrorCode,
    audio_physical::AudioAggregateLedger,
    sync_reply::{AudioWirePart, SyncReplyBytes},
};
use std::sync::Arc;
pub(crate) fn lifetime() {
    let values = [-0.0f32, 1.5, f32::INFINITY, f32::from_bits(0x7fc00003)];
    let header = [7u8; 16];
    let first = [1u8, 2, 3, 4];
    let middle = [9u8; 4];
    let want = [
        first.to_vec(),
        values.iter().flat_map(|f| f.to_le_bytes()).collect(),
        middle.to_vec(),
        values.iter().rev().flat_map(|f| f.to_le_bytes()).collect(),
    ]
    .concat();
    let reverse = values.into_iter().rev().collect::<Vec<_>>();
    let total = 16 + want.len();
    let ledger = Arc::new(AudioAggregateLedger::new(total));
    let parts = [
        AudioWirePart::Bytes(&first),
        AudioWirePart::Floats(&values),
        AudioWirePart::Bytes(&middle),
        AudioWirePart::Floats(&reverse),
    ];
    let mut reply = SyncReplyBytes::encode_audio(&ledger, 16, &parts).unwrap();
    assert_eq!(
        reply.len(),
        want.len(),
        "wire reply exposed reserved headroom"
    );
    assert_eq!(
        reply.as_ref(),
        want,
        "audio wire encoding changed complete LE sections"
    );
    assert_eq!(
        reply.capacity(),
        total,
        "audio wire storage exceeded charged capacity"
    );
    assert_eq!(
        ledger.used_bytes(),
        total,
        "audio wire final capacity was not charged"
    );
    assert!(
        reply.is_charged(),
        "audio wire reply lost its charge marker"
    );
    let pointer = reply.as_ptr();
    let before = migo_alloc_probe::thread_counts();
    reply.prepend(&header).unwrap();
    assert_eq!(
        migo_alloc_probe::thread_counts().bytes_allocated,
        before.bytes_allocated,
        "wire envelope allocated replacement storage"
    );
    assert_eq!(
        reply.as_ptr().wrapping_add(16),
        pointer,
        "prepended envelope copied wire allocation"
    );
    assert_eq!(reply.len(), total, "wire envelope was not revealed");
    assert_eq!(&reply[..16], &header);
    assert_eq!(&reply[16..], want);
    assert!(
        reply.prepend(&[0]).is_err(),
        "wire header reveal exceeded reserved capacity"
    );
    assert_eq!(
        ledger.used_bytes(),
        total,
        "wire header reveal returned allocation credit"
    );
    assert!(
        ledger.try_reserve(1, "peer audio host").is_err(),
        "live wire reply reopened physical admission"
    );
    drop(reply);
    assert_eq!(
        ledger.used_bytes(),
        0,
        "wire final owner leaked physical credit"
    );
    let legacy = SyncReplyBytes::from(vec![1, 2, 3]);
    assert!(!legacy.is_charged());
    assert_eq!(legacy.as_ref(), [1, 2, 3]);
    refused_before_allocation();
}
fn refused_before_allocation() {
    let values = vec![1.25f32; 1 << 20];
    let ledger = Arc::new(AudioAggregateLedger::new(values.len() * 4 + 15));
    let before = migo_alloc_probe::thread_counts();
    let result = SyncReplyBytes::encode_audio(&ledger, 16, &[AudioWirePart::Floats(&values)]);
    assert!(
        matches!(result,Err(e)if e.code==ErrorCode::InputSaturated),
        "audio wire output bypassed physical refusal"
    );
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < 65536,
        "audio wire refusal allocated output before admission"
    );
    assert_eq!(ledger.used_bytes(), 0, "audio wire refusal leaked credit");
}
