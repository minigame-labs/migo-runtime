use audio::nodes::{AudioNodeProcessor, DelayNode};
use std::{hint::black_box, time::Instant};
fn main() {
    for channels in [1u32, 32] {
        for kind in [0, 1, 2] {
            let mut node = DelayNode::new(1, 1.0, 48000, channels);
            let mut data = vec![0.0; 48000 * channels as usize];
            if kind > 0 {
                data[0] = 1.0;
            }
            let count = if kind == 2 { 36000 } else { 128 };
            let mut out = vec![0.0; count * channels as usize];
            node.process(
                &data[..count * channels as usize],
                &mut out,
                48000,
                channels,
                0.0,
            );
            let expected = kind > 0;
            for _ in 0..10 {
                assert_eq!(node.has_tail_audio(), expected);
            }
            let iterations = if kind == 2 { 1000 } else { 10000 };
            let t = Instant::now();
            for _ in 0..iterations {
                assert_eq!(black_box(&node).has_tail_audio(), expected);
            }
            println!(
                "{} {} {} {}",
                channels,
                kind,
                iterations,
                t.elapsed().as_nanos()
            );
        }
    }
}
