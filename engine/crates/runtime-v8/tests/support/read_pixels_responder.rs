//! A render-channel fixture, not a GL driver. Real PACK/driver coverage lives
//! in the graphics crate. This fixture checks the JS/native offset boundary.
use shared::protocol::{
    pixel_pack::PixelPackLayout,
    render_cmd::{GLCmd, RenderCommand, webgl_readback_bytes_per_pixel},
};

pub fn spawn(
    rx: crossbeam_channel::Receiver<RenderCommand>,
) -> std::thread::JoinHandle<Vec<usize>> {
    std::thread::spawn(move || {
        let mut lengths = Vec::new();
        for command in rx {
            match command {
                RenderCommand::GL(GLCmd::ReadPixels {
                    width,
                    height,
                    format,
                    type_,
                    destination_byte_length,
                    resp,
                    ..
                }) => {
                    lengths.push(destination_byte_length);
                    let bpp = webgl_readback_bytes_per_pixel(format, type_).unwrap();
                    let layout = PixelPackLayout::new(width, height, bpp, 4, 0, 0, 0).unwrap();
                    assert!(layout.required_bytes <= destination_byte_length);
                    resp.ok(shared::protocol::render_cmd::ReadPixelsData {
                        pixels: (1..=layout.compact_bytes).map(|x| x as u8).collect(),
                        layout,
                    });
                }
                RenderCommand::FramePacket(_) => {}
                other => panic!("unexpected readback command: {other:?}"),
            }
        }
        lengths
    })
}

pub fn assert_lengths(lengths: Vec<usize>) {
    // Six typed-array brands, each with normal and shared backing; then numeric
    // conversion cases, omitted offset, valueOf, zero-area end, and WebGL1.
    let mut expected = vec![61, 61, 61, 61, 58, 58, 52, 52, 52, 52, 52, 52];
    expected.extend([16, 16, 16, 16, 16, 16, 16, 14, 13, 15, 16, 16]);
    expected.extend([8, 6, 0, 8]);
    assert_eq!(lengths, expected, "rejected reads must not reach renderer");
}
