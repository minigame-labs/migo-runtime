//! Only injected into the private native-instrumented single-test program.
//! The full product suite runs without process-global C test counters.
use crate::decoder::mp3::Mp3DecodeJob;
use shared::error::ErrorCode;
use std::sync::{Arc, atomic::AtomicBool};
unsafe extern "C" {
    fn migo_mp3_test_live_allocations() -> usize;
}
fn live() -> usize {
    unsafe { migo_mp3_test_live_allocations() }
}
const MUSIC: &[u8] = include_bytes!(concat!(env!("MIGO_MP3_TEST_FIXTURES"), "/music.mp3"));

#[test]
fn native_owner_releases_on_real_job_cancellation_eof_and_error() {
    for steps in [0, 1, 2, 3, 10, 100, 1000] {
        let start = live();
        let input: Arc<[u8]> = MUSIC.into();
        let weak = Arc::downgrade(&input);
        let mut job = Mp3DecodeJob::new(input, Some(96000));
        for _ in 0..steps {
            if job.step(&AtomicBool::new(false)).unwrap() {
                break;
            }
        }
        if (2..1000).contains(&steps) {
            assert_eq!(live(), start + 1, "one real native owner must be live");
        }
        assert!(
            matches!(job.step(&AtomicBool::new(true)), Err(e) if e.code == ErrorCode::Cancelled)
        );
        assert_eq!(
            live(),
            start,
            "the real job must release its native C allocation"
        );
        assert!(weak.upgrade().is_none());
        assert!(job.finish().is_err());
    }
    for complete in [false, true] {
        let start = live();
        let bytes = if complete {
            MUSIC
        } else {
            &MUSIC[..MUSIC.len() / 2]
        };
        let input: Arc<[u8]> = bytes.into();
        let weak = Arc::downgrade(&input);
        let mut job = Mp3DecodeJob::new(input, None);
        let mut terminal = false;
        for _ in 0..10000 {
            match job.step(&AtomicBool::new(false)) {
                Ok(true) => {
                    assert!(complete);
                    assert_eq!(job.finish().unwrap().samples.len(), 960274);
                    terminal = true;
                    break;
                }
                Ok(false) => {}
                Err(e) => {
                    assert!(!complete && e.code == ErrorCode::InvalidArgument);
                    assert!(job.finish().is_err());
                    terminal = true;
                    break;
                }
            }
        }
        assert!(terminal && weak.upgrade().is_none());
        assert_eq!(live(), start);
    }
}

#[test]
fn injected_native_allocation_failure_retires_without_partial_pcm() {
    let start = live();
    let input: Arc<[u8]> = MUSIC.into();
    let weak = Arc::downgrade(&input);
    let mut job = Mp3DecodeJob::new(input, None);
    for _ in 0..10000 {
        match job.step(&AtomicBool::new(false)) {
            Err(e) => {
                assert_eq!(e.code, ErrorCode::OutOfMemory);
                assert!(weak.upgrade().is_none());
                assert!(matches!(job.finish(), Err(e) if e.code == ErrorCode::OutOfMemory));
                assert_eq!(live(), start);
                return;
            }
            Ok(true) => panic!("the explicit allocation-failure cohort must fail allocation"),
            Ok(false) => {}
        }
    }
    panic!("allocation failure did not retire");
}
