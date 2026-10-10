//! Characteristic values from the peripherals a host connects for content.
//!
//! Every Bluetooth operation goes through the host-service channel
//! (`MIGO_HOST_SERVICE_BLUETOOTH`); what a connected peripheral sends comes back
//! here, typed, because notifications arrive at whatever rate the peripheral
//! chooses. The record is borrowed for the call and its bytes copied once, into a
//! pooled slot -- the same path Android's notifications take.

use migo_capi_abi::{
    MIGO_ERROR_INVALID_ARGUMENT, MigoResult,
    host_services::{MIGO_HOST_SERVICE_BLUETOOTH, MigoBleCharacteristicValue},
};

use crate::{MigoSession, map_ingress_result, panic_barrier::guard, pin_session};

/// # Safety
/// `session` must be a live session handle and `value` null or readable for its
/// announced size, with every range it announces readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_post_ble_characteristic_value(
    session: *mut MigoSession,
    value: *const MigoBleCharacteristicValue,
) -> MigoResult {
    guard("migo_session_post_ble_characteristic_value", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let value = match unsafe { MigoBleCharacteristicValue::parse(value) } {
            Ok(value) => value,
            Err(error) => return error,
        };
        let declared = session.state.lock().is_ok_and(|state| {
            state.callbacks.as_ref().is_some_and(|callbacks| {
                callbacks.supplies_host_service(MIGO_HOST_SERVICE_BLUETOOTH)
            })
        });
        if !declared {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        let ingress = match session.active_ingress() {
            Ok(ingress) => ingress,
            Err(error) => return error,
        };
        map_ingress_result(
            &session,
            "migo_session_post_ble_characteristic_value",
            ingress.try_send_ble_characteristic_value(
                value.device_id,
                value.service_id,
                value.characteristic_id,
                value.value,
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use migo_capi_abi::{
        MIGO_ABI_VERSION_CURRENT, VersionedHeader,
        host_services::{BleCharacteristicValueView, MIGO_BLE_VALUE_MAX_BYTES},
    };

    use super::*;

    fn record(device: &str, value: &[u8]) -> MigoBleCharacteristicValue {
        const SERVICE: &str = "0000ffe0-0000-1000-8000-00805f9b34fb";
        const CHARACTERISTIC: &str = "0000ffe1-0000-1000-8000-00805f9b34fb";
        MigoBleCharacteristicValue {
            header: VersionedHeader {
                struct_size: std::mem::size_of::<MigoBleCharacteristicValue>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            device_id_utf8: device.as_ptr().cast(),
            service_id_utf8: SERVICE.as_ptr().cast(),
            characteristic_id_utf8: CHARACTERISTIC.as_ptr().cast(),
            value: value.as_ptr(),
            device_id_length: device.len() as u32,
            service_id_length: SERVICE.len() as u32,
            characteristic_id_length: CHARACTERISTIC.len() as u32,
            value_length: value.len() as u32,
        }
    }

    #[test]
    fn a_value_is_borrowed_as_the_host_sent_it() {
        let bytes = [0x01, 0x02, 0xff];
        let raw = record("AA:BB:CC:DD:EE:FF", &bytes);
        let view: BleCharacteristicValueView<'_> =
            unsafe { MigoBleCharacteristicValue::parse(&raw) }.unwrap();
        assert_eq!(view.device_id, "AA:BB:CC:DD:EE:FF");
        assert_eq!(view.value, &bytes);
    }

    #[test]
    fn an_id_or_value_out_of_bounds_is_refused() {
        let empty_device = record("", &[1]);
        let oversized = vec![0u8; MIGO_BLE_VALUE_MAX_BYTES as usize + 1];
        let too_long = record("AA:BB", &oversized);
        let mut not_utf8 = record("AA:BB", &[1]);
        let bad = [0xffu8, 0xfe];
        not_utf8.device_id_utf8 = bad.as_ptr().cast();
        not_utf8.device_id_length = 2;
        for raw in [empty_device, too_long, not_utf8] {
            assert_eq!(
                unsafe { MigoBleCharacteristicValue::parse(&raw) },
                Err(MIGO_ERROR_INVALID_ARGUMENT)
            );
        }
    }
}
