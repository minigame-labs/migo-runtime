use std::any::Any;

use shared::protocol::audio_cmd::AudioNodeId;

use crate::param::AudioParamTimeline;

use super::AudioNodeProcessor;

/// Panning model for PannerNode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanningModel {
    EqualPower,
    HRTF,
}

impl PanningModel {
    pub fn from_str(s: &str) -> Self {
        match s {
            "HRTF" => Self::HRTF,
            _ => Self::EqualPower,
        }
    }
}

/// Distance model for PannerNode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceModel {
    Linear,
    Inverse,
    Exponential,
}

impl DistanceModel {
    pub fn from_str(s: &str) -> Self {
        match s {
            "linear" => Self::Linear,
            "exponential" => Self::Exponential,
            _ => Self::Inverse,
        }
    }
}

/// PannerNode: spatializes audio using equal-power panning.
///
/// Uses 3D position and orientation to compute stereo pan.
/// HRTF mode falls back to equal-power for now.
pub struct PannerNode {
    id: AudioNodeId,
    panning_model: PanningModel,
    distance_model: DistanceModel,
    position_x: AudioParamTimeline,
    position_y: AudioParamTimeline,
    position_z: AudioParamTimeline,
    orientation_x: AudioParamTimeline,
    orientation_y: AudioParamTimeline,
    orientation_z: AudioParamTimeline,
    ref_distance: f64,
    max_distance: f64,
    rolloff_factor: f64,
    cone_inner_angle: f64,
    cone_outer_angle: f64,
    cone_outer_gain: f64,
    // Cached geometry so the trigonometry is only redone when the source moves.
    cached_position: [f64; 3],
    cached_orientation: [f64; 3],
    cached_azimuth: f64,
    cached_gain_l: f32,
    cached_gain_r: f32,
    /// Distance and cone gain. Kept apart from the pan gains on purpose: the
    /// stereo equal-power rules pass one input channel through unmultiplied, so
    /// folding attenuation into the pan gains would leave that channel at full
    /// level and a distant stereo source would never get quieter.
    cached_attenuation: f32,
}

impl PannerNode {
    pub fn new(id: AudioNodeId) -> Self {
        Self {
            id,
            panning_model: PanningModel::EqualPower,
            distance_model: DistanceModel::Inverse,
            position_x: AudioParamTimeline::new(0.0, f32::MIN, f32::MAX),
            position_y: AudioParamTimeline::new(0.0, f32::MIN, f32::MAX),
            position_z: AudioParamTimeline::new(0.0, f32::MIN, f32::MAX),
            orientation_x: AudioParamTimeline::new(1.0, f32::MIN, f32::MAX),
            orientation_y: AudioParamTimeline::new(0.0, f32::MIN, f32::MAX),
            orientation_z: AudioParamTimeline::new(0.0, f32::MIN, f32::MAX),
            ref_distance: 1.0,
            max_distance: 10000.0,
            rolloff_factor: 1.0,
            cone_inner_angle: 360.0,
            cone_outer_angle: 360.0,
            cone_outer_gain: 0.0,
            // NaN forces the first computation.
            cached_position: [f64::NAN; 3],
            cached_orientation: [f64::NAN; 3],
            cached_azimuth: 0.0,
            cached_gain_l: std::f32::consts::FRAC_1_SQRT_2,
            cached_gain_r: std::f32::consts::FRAC_1_SQRT_2,
            cached_attenuation: 1.0,
        }
    }

    pub fn set_panning_model(&mut self, model: PanningModel) {
        self.panning_model = model;
    }

    pub fn set_distance_model(&mut self, model: DistanceModel) {
        self.distance_model = model;
    }

    pub fn set_ref_distance(&mut self, v: f64) {
        self.ref_distance = v.max(0.0);
    }

    pub fn set_max_distance(&mut self, v: f64) {
        self.max_distance = v.max(0.0);
    }

    pub fn set_rolloff_factor(&mut self, v: f64) {
        self.rolloff_factor = v.max(0.0);
    }

    pub fn set_cone_inner_angle(&mut self, v: f64) {
        self.cone_inner_angle = v;
    }

    pub fn set_cone_outer_angle(&mut self, v: f64) {
        self.cone_outer_angle = v;
    }

    pub fn set_cone_outer_gain(&mut self, v: f64) {
        self.cone_outer_gain = v.clamp(0.0, 1.0);
    }

    /// Distance attenuation for the source's distance from the listener.
    fn compute_distance_gain(&self, distance: f64) -> f64 {
        match self.distance_model {
            DistanceModel::Linear => {
                let d = distance.clamp(self.ref_distance, self.max_distance);
                if self.max_distance == self.ref_distance {
                    1.0
                } else {
                    // Clamped at zero: the linear model goes negative past
                    // maxDistance for rolloff > 1, and a negative gain is a phase
                    // inversion, not attenuation.
                    (1.0 - self.rolloff_factor * (d - self.ref_distance)
                        / (self.max_distance - self.ref_distance))
                        .max(0.0)
                }
            }
            DistanceModel::Inverse => {
                let d = distance.max(self.ref_distance);
                if self.ref_distance == 0.0 {
                    1.0
                } else {
                    self.ref_distance
                        / (self.ref_distance + self.rolloff_factor * (d - self.ref_distance))
                }
            }
            DistanceModel::Exponential => {
                let d = distance.max(self.ref_distance);
                if self.ref_distance == 0.0 {
                    1.0
                } else {
                    (d / self.ref_distance).powf(-self.rolloff_factor)
                }
            }
        }
    }

    /// Attenuation from the source's directivity cone.
    ///
    /// **`orientation*` and the `cone*` properties used to be write-only.** They
    /// were settable, stored, and never read, so a game aiming a directional source
    /// heard no difference at all. The cone is the whole reason a PannerNode has an
    /// orientation.
    fn compute_cone_gain(&self, source_to_listener: [f64; 3], orientation: [f64; 3]) -> f64 {
        let orientation_length = length(orientation);
        if orientation_length == 0.0
            || (self.cone_inner_angle == 360.0 && self.cone_outer_angle == 360.0)
        {
            return 1.0;
        }

        let listener_length = length(source_to_listener);
        if listener_length == 0.0 {
            return 1.0;
        }

        let cosine = (dot(source_to_listener, orientation)
            / (listener_length * orientation_length))
            .clamp(-1.0, 1.0);
        let angle = cosine.acos().to_degrees().abs();
        let inner = (self.cone_inner_angle / 2.0).abs();
        let outer = (self.cone_outer_angle / 2.0).abs();

        if angle <= inner {
            1.0
        } else if angle >= outer {
            self.cone_outer_gain
        } else {
            // Linear in angle between the two cones, per the spec.
            let progress = (angle - inner) / (outer - inner);
            1.0 + (self.cone_outer_gain - 1.0) * progress
        }
    }

    /// Azimuth of the source in the listener's frame, in degrees, in [-180, 180].
    ///
    /// The listener sits at the origin looking down -Z with +Y up, which makes +X
    /// its right. This used to be `asin(x / distance)`, which is not the spec's
    /// azimuth and in particular cannot tell front from back: a source directly
    /// behind the listener panned identically to one directly in front.
    fn azimuth(position: [f64; 3]) -> f64 {
        const FORWARD: [f64; 3] = [0.0, 0.0, -1.0];
        const UP: [f64; 3] = [0.0, 1.0, 0.0];
        const RIGHT: [f64; 3] = [1.0, 0.0, 0.0];

        let distance = length(position);
        if distance == 0.0 {
            return 0.0;
        }
        let direction = [
            position[0] / distance,
            position[1] / distance,
            position[2] / distance,
        ];

        // Project onto the horizontal plane; elevation does not affect equal-power
        // panning.
        let vertical = dot(direction, UP);
        let projected = [
            direction[0] - vertical * UP[0],
            direction[1] - vertical * UP[1],
            direction[2] - vertical * UP[2],
        ];
        let projected_length = length(projected);
        if projected_length == 0.0 {
            // Directly overhead or underneath: dead centre.
            return 0.0;
        }
        let horizontal = [
            projected[0] / projected_length,
            projected[1] / projected_length,
            projected[2] / projected_length,
        ];

        // Signed angle from straight ahead, positive to the right.
        let right = dot(horizontal, RIGHT);
        let front = dot(horizontal, FORWARD);
        right.atan2(front).to_degrees()
    }
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn length(v: [f64; 3]) -> f64 {
    dot(v, v).sqrt()
}

impl AudioNodeProcessor for PannerNode {
    fn id(&self) -> AudioNodeId {
        self.id
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn process(
        &mut self,
        inputs: &[f32],
        output: &mut [f32],
        _sample_rate: u32,
        channels: u32,
        current_time: f64,
    ) -> usize {
        let ch = channels.max(1) as usize;
        let len = inputs.len().min(output.len());
        if len == 0 {
            return 0;
        }

        let position = [
            self.position_x.compute_value(current_time) as f64,
            self.position_y.compute_value(current_time) as f64,
            self.position_z.compute_value(current_time) as f64,
        ];
        let orientation = [
            self.orientation_x.compute_value(current_time) as f64,
            self.orientation_y.compute_value(current_time) as f64,
            self.orientation_z.compute_value(current_time) as f64,
        ];

        // The trigonometry is only redone when the geometry moves, which for a
        // static emitter is once.
        if position != self.cached_position || orientation != self.cached_orientation {
            self.cached_position = position;
            self.cached_orientation = orientation;

            let distance = length(position);
            let distance_gain = self.compute_distance_gain(distance);
            // Listener at the origin, so listener - source is just -position.
            let source_to_listener = [-position[0], -position[1], -position[2]];
            let cone_gain = self.compute_cone_gain(source_to_listener, orientation);
            self.cached_attenuation = (distance_gain * cone_gain) as f32;

            let azimuth = Self::azimuth(position);
            self.cached_azimuth = azimuth;

            // Equal-power pan position. A mono source spans the full arc; a stereo
            // one keeps its own image and is pushed toward whichever side the
            // azimuth points at, per the spec's stereo equalpower rules.
            let normalized = if ch == 1 {
                (azimuth.clamp(-90.0, 90.0) + 90.0) / 180.0
            } else if azimuth <= 0.0 {
                (azimuth.max(-90.0) + 90.0) / 90.0
            } else {
                azimuth.min(90.0) / 90.0
            };
            let angle = normalized * std::f64::consts::FRAC_PI_2;
            self.cached_gain_l = angle.cos() as f32;
            self.cached_gain_r = angle.sin() as f32;
        }

        let gain_l = self.cached_gain_l;
        let gain_r = self.cached_gain_r;
        let attenuation = self.cached_attenuation;
        let azimuth = self.cached_azimuth;
        let frames = len / ch;

        if ch == 1 {
            // A mono bus has no second channel to pan into, so only the distance
            // and cone attenuation is meaningful here.
            for frame in 0..frames {
                output[frame] = inputs[frame] * attenuation;
            }
            return frames;
        }

        for frame in 0..frames {
            let base = frame * ch;
            let left = inputs[base];
            let right = inputs[base + 1];
            // Stereo equal-power: the channel being panned away from is folded into
            // the near side rather than discarded, so a hard pan keeps the energy.
            // Downmixing to mono first (what this used to do) collapsed every stereo
            // source's image the moment it was spatialised.
            let (out_l, out_r) = if azimuth <= 0.0 {
                (left + right * gain_l, right * gain_r)
            } else {
                (left * gain_l, right + left * gain_r)
            };
            output[base] = out_l * attenuation;
            output[base + 1] = out_r * attenuation;
            // Surround channels carry no spatialised signal.
            output[base + 2..base + ch].fill(0.0);
        }

        frames
    }

    fn get_param_mut(&mut self, name: &str) -> Option<&mut AudioParamTimeline> {
        match name {
            "positionX" => Some(&mut self.position_x),
            "positionY" => Some(&mut self.position_y),
            "positionZ" => Some(&mut self.position_z),
            "orientationX" => Some(&mut self.orientation_x),
            "orientationY" => Some(&mut self.orientation_y),
            "orientationZ" => Some(&mut self.orientation_z),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panned(position: [f32; 3], input: &[f32], channels: u32) -> Vec<f32> {
        let mut node = PannerNode::new(1);
        node.position_x.set_value(position[0]);
        node.position_y.set_value(position[1]);
        node.position_z.set_value(position[2]);
        let mut out = vec![0.0f32; input.len()];
        node.process(input, &mut out, 48_000, channels, 0.0);
        out
    }

    /// `asin(x / distance)` cannot tell front from back: both map to azimuth 0, so
    /// a source behind the listener panned exactly like one in front. The listener
    /// looks down -Z, so -Z is ahead and +Z is behind.
    #[test]
    fn azimuth_distinguishes_front_from_back() {
        assert!(PannerNode::azimuth([0.0, 0.0, -1.0]).abs() < 1e-9, "ahead");
        assert!(
            (PannerNode::azimuth([0.0, 0.0, 1.0]).abs() - 180.0).abs() < 1e-9,
            "behind"
        );
        assert!(
            (PannerNode::azimuth([1.0, 0.0, 0.0]) - 90.0).abs() < 1e-9,
            "right"
        );
        assert!(
            (PannerNode::azimuth([-1.0, 0.0, 0.0]) + 90.0).abs() < 1e-9,
            "left"
        );
    }

    #[test]
    fn a_source_to_the_right_is_louder_on_the_right() {
        // Unit distance so distance attenuation is 1 with the default ref distance.
        let out = panned([1.0, 0.0, 0.0], &[1.0, 1.0], 2);
        assert!(out[1] > out[0], "right channel must dominate: {out:?}");
    }

    #[test]
    fn a_source_to_the_left_is_louder_on_the_left() {
        let out = panned([-1.0, 0.0, 0.0], &[1.0, 1.0], 2);
        assert!(out[0] > out[1], "left channel must dominate: {out:?}");
    }

    /// A stereo source used to be summed to mono before panning, which collapsed
    /// its image the moment it was spatialised. Centred, it must come out unchanged.
    #[test]
    fn a_centred_stereo_source_keeps_its_image() {
        let out = panned([0.0, 0.0, -1.0], &[1.0, -1.0], 2);
        assert!(
            (out[0] - 1.0).abs() < 1e-5 && (out[1] + 1.0).abs() < 1e-5,
            "a centred stereo source must pass through: {out:?}"
        );
    }

    /// The cone properties were settable and never read. A source pointed away from
    /// the listener has to be quieter than one pointed at it.
    #[test]
    fn orientation_drives_the_cone_gain() {
        let mut facing = PannerNode::new(1);
        facing.set_cone_inner_angle(30.0);
        facing.set_cone_outer_angle(90.0);
        facing.set_cone_outer_gain(0.0);
        facing.position_z.set_value(-1.0);
        // Pointing at the listener, who is at the origin.
        facing.orientation_x.set_value(0.0);
        facing.orientation_z.set_value(1.0);

        let mut away = PannerNode::new(1);
        away.set_cone_inner_angle(30.0);
        away.set_cone_outer_angle(90.0);
        away.set_cone_outer_gain(0.0);
        away.position_z.set_value(-1.0);
        // Pointing directly away.
        away.orientation_x.set_value(0.0);
        away.orientation_z.set_value(-1.0);

        let mut toward_out = [0.0f32; 2];
        facing.process(&[1.0, 1.0], &mut toward_out, 48_000, 2, 0.0);
        let mut away_out = [0.0f32; 2];
        away.process(&[1.0, 1.0], &mut away_out, 48_000, 2, 0.0);

        let toward_level = toward_out[0].abs() + toward_out[1].abs();
        let away_level = away_out[0].abs() + away_out[1].abs();
        assert!(
            away_level < toward_level,
            "a source aimed away must be attenuated: {away_level} vs {toward_level}"
        );
    }

    #[test]
    fn distance_attenuates_and_the_linear_model_never_inverts_phase() {
        let near = panned([0.0, 0.0, -1.0], &[1.0, 1.0], 2);
        let far = panned([0.0, 0.0, -100.0], &[1.0, 1.0], 2);
        assert!(far[0].abs() < near[0].abs(), "distance must attenuate");

        let mut node = PannerNode::new(1);
        node.set_distance_model(DistanceModel::Linear);
        node.set_ref_distance(1.0);
        node.set_max_distance(10.0);
        node.set_rolloff_factor(5.0); // large enough to drive the formula negative
        assert!(
            node.compute_distance_gain(1000.0) >= 0.0,
            "the linear model must clamp at silence, not invert"
        );
    }
}
