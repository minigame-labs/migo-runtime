//! Readings from the motion sensors a host runs for content.
//!
//! The sensors start and stop through the host-service channel
//! (`MIGO_HOST_SERVICE_MOTION`); their readings come back here, typed, because
//! they arrive up to fifty times a second each. They take the same bounded
//! ingress as input and become the same commands Android's sensor callbacks
//! send, so content sees one stream whichever SDK produced it.

use std::borrow::Cow;
use std::num::NonZeroI64;

use migo_capi_abi::{
    MIGO_ERROR_INVALID_ARGUMENT, MigoResult,
    host_services::{
        MIGO_COMPASS_ACCURACY_HIGH, MIGO_COMPASS_ACCURACY_LOW, MIGO_COMPASS_ACCURACY_MEDIUM,
        MIGO_COMPASS_ACCURACY_NO_CONTACT, MIGO_COMPASS_ACCURACY_UNRELIABLE,
        MIGO_HOST_SERVICE_MOTION, MIGO_SENSOR_ACCELEROMETER, MIGO_SENSOR_COMPASS,
        MIGO_SENSOR_DEVICE_MOTION, MIGO_SENSOR_GYROSCOPE, MigoSensorSample,
    },
};
use shared::protocol::host_cmd::HostCommand;

use crate::{MigoSession, map_ingress_result, panic_barrier::guard, pin_session};

/// The words content's `onCompassChange` reports accuracy in.
fn compass_accuracy(accuracy: u32) -> &'static str {
    match accuracy {
        MIGO_COMPASS_ACCURACY_HIGH => "high",
        MIGO_COMPASS_ACCURACY_MEDIUM => "medium",
        MIGO_COMPASS_ACCURACY_LOW => "low",
        MIGO_COMPASS_ACCURACY_NO_CONTACT => "no-contact",
        MIGO_COMPASS_ACCURACY_UNRELIABLE => "unreliable",
        _ => "unknown",
    }
}

/// The command a validated reading becomes, fenced to the runtime generation it
/// was taken for: a reading queued before a restart does not reach the content
/// that replaced it.
fn sample_command(sample: &MigoSensorSample, generation: Option<NonZeroI64>) -> HostCommand {
    let [first, second, third] = sample.values;
    match sample.kind {
        MIGO_SENSOR_ACCELEROMETER => HostCommand::OnAccelerometerChange {
            x: first,
            y: second,
            z: third,
            runtime_generation: generation,
        },
        MIGO_SENSOR_GYROSCOPE => HostCommand::OnGyroscopeChange {
            x: first,
            y: second,
            z: third,
            runtime_generation: generation,
        },
        MIGO_SENSOR_DEVICE_MOTION => HostCommand::OnDeviceMotionChange {
            alpha: first,
            beta: second,
            gamma: third,
            runtime_generation: generation,
        },
        _ => {
            debug_assert_eq!(
                sample.kind, MIGO_SENSOR_COMPASS,
                "parse admits no other kind"
            );
            HostCommand::OnCompassChange {
                direction: first,
                accuracy: Cow::Borrowed(compass_accuracy(sample.compass_accuracy)),
                runtime_generation: generation,
            }
        }
    }
}

/// # Safety
/// `session` must be a live session handle and `sample` null or readable for its
/// announced size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_post_sensor_sample(
    session: *mut MigoSession,
    sample: *const MigoSensorSample,
) -> MigoResult {
    guard("migo_session_post_sensor_sample", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let sample = match unsafe { MigoSensorSample::parse(sample) } {
            Ok(sample) => sample,
            Err(error) => return error,
        };
        // Readings belong to sensors content started through the motion service;
        // a host that did not declare it has none to report.
        let declared = session.state.lock().is_ok_and(|state| {
            state
                .callbacks
                .as_ref()
                .is_some_and(|callbacks| callbacks.supplies_host_service(MIGO_HOST_SERVICE_MOTION))
        });
        if !declared {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        let ingress = match session.active_ingress() {
            Ok(ingress) => ingress,
            Err(error) => return error,
        };
        let command = sample_command(&sample, NonZeroI64::new(ingress.runtime_generation()));
        map_ingress_result(
            &session,
            "migo_session_post_sensor_sample",
            ingress.try_send(command),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_capi_abi::{MIGO_ABI_VERSION_CURRENT, VersionedHeader};

    fn sample(kind: u32, accuracy: u32, values: [f64; 3]) -> MigoSensorSample {
        MigoSensorSample {
            header: VersionedHeader {
                struct_size: std::mem::size_of::<MigoSensorSample>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            kind,
            compass_accuracy: accuracy,
            values,
        }
    }

    fn parse(sample: &MigoSensorSample) -> Result<MigoSensorSample, MigoResult> {
        unsafe { MigoSensorSample::parse(sample) }
    }

    #[test]
    fn each_kind_becomes_the_command_android_sends() {
        let generation = NonZeroI64::new(3);
        let accel = parse(&sample(MIGO_SENSOR_ACCELEROMETER, 0, [0.0, 0.0, 1.0])).unwrap();
        assert!(matches!(
            sample_command(&accel, generation),
            HostCommand::OnAccelerometerChange { z, runtime_generation, .. }
                if z == 1.0 && runtime_generation == generation
        ));
        let compass = parse(&sample(
            MIGO_SENSOR_COMPASS,
            MIGO_COMPASS_ACCURACY_NO_CONTACT,
            [271.5, 0.0, 0.0],
        ))
        .unwrap();
        assert!(matches!(
            sample_command(&compass, generation),
            HostCommand::OnCompassChange { direction, ref accuracy, .. }
                if direction == 271.5 && accuracy == "no-contact"
        ));
    }

    #[test]
    fn a_reading_the_header_does_not_define_is_refused() {
        for bad in [
            sample(4, 0, [0.0; 3]),
            sample(MIGO_SENSOR_GYROSCOPE, MIGO_COMPASS_ACCURACY_HIGH, [0.0; 3]),
            sample(MIGO_SENSOR_COMPASS, 6, [0.0; 3]),
            sample(MIGO_SENSOR_COMPASS, 0, [10.0, 1.0, 0.0]),
            sample(MIGO_SENSOR_ACCELEROMETER, 0, [f64::NAN, 0.0, 0.0]),
            sample(MIGO_SENSOR_DEVICE_MOTION, 0, [f64::INFINITY, 0.0, 0.0]),
        ] {
            assert_eq!(parse(&bad), Err(MIGO_ERROR_INVALID_ARGUMENT), "{bad:?}");
        }
    }
}
