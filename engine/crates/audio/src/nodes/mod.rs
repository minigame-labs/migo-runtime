mod analyser;
mod biquad_filter;
mod buffer_source;
mod channel_merger;
mod channel_splitter;
mod constant_source;
mod delay;
mod destination;
mod dynamics_compressor;
mod gain;
mod iir_filter;
mod oscillator;
mod panner;
mod wave_shaper;

pub use analyser::AnalyserNode;
pub use biquad_filter::{BiquadFilterNode, BiquadFilterType};
pub use buffer_source::BufferSourceNode;
pub use channel_merger::ChannelMergerNode;
pub use channel_splitter::ChannelSplitterNode;
pub use constant_source::ConstantSourceNode;
pub use delay::DelayNode;
pub use destination::DestinationNode;
pub use dynamics_compressor::DynamicsCompressorNode;
pub use gain::GainNode;
pub use iir_filter::IIRFilterNode;
pub use oscillator::{OscillatorNode, OscillatorType};
pub use panner::{DistanceModel, PannerNode, PanningModel};
pub use wave_shaper::{OversampleType, WaveShaperNode};

use std::any::Any;

use shared::protocol::audio_cmd::AudioNodeId;

use crate::param::AudioParamTimeline;

/// Trait for all audio processing nodes in the audio graph.
///
/// Each node receives mixed input from upstream connections and writes its
/// output to the provided buffer. Source nodes (BufferSource, Oscillator, etc.)
/// ignore inputs and generate audio. Processing nodes (Gain, BiquadFilter, etc.)
/// transform their input. The Destination node is the final output.
pub trait AudioNodeProcessor: Send + 'static {
    /// Get the unique node ID
    fn id(&self) -> AudioNodeId;

    /// Downcast support: return self as Any for type-specific operations
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Process audio.
    ///
    /// - `inputs`: mixed audio from all upstream connected nodes (interleaved samples).
    ///   Empty slice for source nodes with no upstream connections.
    /// - `output`: buffer to write this node's output to (interleaved samples).
    /// - `sample_rate`: the audio context's sample rate.
    /// - `channels`: the number of interleaved channels in `inputs`/`output`.
    /// - `current_time`: the current context time in seconds (for automation).
    ///
    /// Returns the number of frames written to output.
    fn process(
        &mut self,
        inputs: &[f32],
        output: &mut [f32],
        sample_rate: u32,
        channels: u32,
        current_time: f64,
    ) -> usize;

    /// Check if the node has finished (for source nodes that have a finite lifetime)
    fn is_finished(&self) -> bool {
        false
    }

    /// Whether this node can still put audio onto its output on some future
    /// block.
    ///
    /// Deliberately not "is a source and has not finished". A source that has never
    /// been started cannot produce anything, and once JavaScript has dropped the
    /// object there is nobody left to start it -- treating it as active pinned
    /// the audio thread to its 5 ms tick and held the output device open for a
    /// node that would never make a sound. This is also what decides how long a
    /// released effect node must be kept: exactly as long as something upstream
    /// can still feed it.
    fn is_producing(&self) -> bool {
        self.has_tail_audio()
    }

    /// Whether a non-source node still has state that can affect a future
    /// output block after its input becomes silent. Tail-time state is part of
    /// liveness: dropping a released effect here would truncate delay/filter
    /// output, while keeping a zero-state node would leak it forever.
    fn has_tail_audio(&self) -> bool {
        false
    }

    /// Get the number of output channels
    #[allow(dead_code)]
    fn output_channels(&self) -> u32 {
        2 // Default stereo
    }

    /// How many separate output ports this node fans out to.
    ///
    /// Only `ChannelSplitterNode` has more than one. A node with multiple ports
    /// emits **one channel per port**, so a connection from it carries a mono
    /// signal taken from the channel its `output` index names -- which is what
    /// makes a splitter a splitter rather than a pass-through.
    fn output_ports(&self) -> u32 {
        1
    }

    /// How many separate input ports this node accepts.
    ///
    /// Only `ChannelMergerNode` has more than one. A connection into such a node
    /// lands in the single channel its `input` index names, instead of being mixed
    /// across the whole bus.
    fn input_ports(&self) -> u32 {
        1
    }

    /// Get a named AudioParam for automation, if this node has one.
    /// Returns None if the param name is not recognized.
    fn get_param_mut(&mut self, _name: &str) -> Option<&mut AudioParamTimeline> {
        None
    }
}

/// Node connection in the audio graph.
///
/// The port indices are what let a splitter and a merger mean anything. Without
/// them every connection was a whole-bus mix, so both nodes could only be
/// pass-throughs: `createChannelSplitter()` returned something that did not split.
#[derive(Debug, Clone)]
pub struct NodeConnection {
    pub src: AudioNodeId,
    /// Output port on `src`. Meaningful only when `src` has more than one.
    pub src_output: u32,
    pub dst: AudioNodeId,
    /// Input port on `dst`. Meaningful only when `dst` has more than one.
    pub dst_input: u32,
}
