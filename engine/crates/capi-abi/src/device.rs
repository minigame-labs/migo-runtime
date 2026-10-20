//! What a host reports about the device: the network, as one record.

use std::mem::size_of;

use crate::{
    AbiStruct, MIGO_ERROR_INVALID_ARGUMENT, MigoResult, VersionedHeader, copy_versioned,
    validate::{validate_flags, validate_reserved},
};

/// `MIGO_NETWORK_*` from `include/migo/session.h`.
pub const MIGO_NETWORK_NONE: u32 = 0;
pub const MIGO_NETWORK_5G: u32 = 6;

/// `MIGO_NETWORK_FLAG_*`: the network reaches the internet; the system routes it
/// through a proxy; the host judges it weak.
pub const MIGO_NETWORK_FLAG_CONNECTED: u32 = 1 << 0;
pub const MIGO_NETWORK_FLAG_SYSTEM_PROXY: u32 = 1 << 1;
pub const MIGO_NETWORK_FLAG_WEAK: u32 = 1 << 2;
const MIGO_NETWORK_FLAGS_KNOWN: u32 =
    MIGO_NETWORK_FLAG_CONNECTED | MIGO_NETWORK_FLAG_SYSTEM_PROXY | MIGO_NETWORK_FLAG_WEAK;

/// The network the device is on, as content's `getNetworkType` describes it.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoNetworkReport {
    pub header: VersionedHeader,
    pub network_type: u32,
    pub flags: u32,
    /// The signal in dBm, or 0 when the host cannot read it.
    pub signal_strength_dbm: i32,
    pub reserved0: u32,
}

// SAFETY: plain integers. The first version is the whole record.
unsafe impl AbiStruct for MigoNetworkReport {
    const MINIMUM_SIZE: usize = size_of::<MigoNetworkReport>();
}

/// A report that names a network content can be told about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidatedNetworkReport {
    /// `MIGO_NETWORK_*`, at most `MIGO_NETWORK_5G`.
    pub network_type: u32,
    pub connected: bool,
    pub system_proxy: bool,
    pub weak: bool,
    /// 0 when unknown, otherwise below it: dBm of a received signal is negative.
    pub signal_strength_dbm: i32,
}

impl ValidatedNetworkReport {
    /// The report `migo_session_set_network_status` makes: the type and whether it
    /// is connected, and nothing more known.
    pub fn status(network_type: u32, connected: bool) -> Result<Self, MigoResult> {
        Self {
            network_type,
            connected,
            system_proxy: false,
            weak: false,
            signal_strength_dbm: 0,
        }
        .checked()
    }

    fn checked(self) -> Result<Self, MigoResult> {
        if self.network_type > MIGO_NETWORK_5G
            || (self.network_type == MIGO_NETWORK_NONE && (self.connected || self.weak))
            || self.signal_strength_dbm > 0
        {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        Ok(self)
    }
}

impl MigoNetworkReport {
    /// # Safety
    /// `report` must be null or readable for its announced byte count.
    pub unsafe fn parse(report: *const Self) -> Result<ValidatedNetworkReport, MigoResult> {
        // SAFETY: forwarded from this function's contract.
        let raw = unsafe { copy_versioned::<Self>(report.cast()) }?;
        raw.validate_fields()
    }

    fn validate_fields(self) -> Result<ValidatedNetworkReport, MigoResult> {
        validate_flags(u64::from(self.flags), u64::from(MIGO_NETWORK_FLAGS_KNOWN))?;
        validate_reserved(u64::from(self.reserved0))?;
        ValidatedNetworkReport {
            network_type: self.network_type,
            connected: self.flags & MIGO_NETWORK_FLAG_CONNECTED != 0,
            system_proxy: self.flags & MIGO_NETWORK_FLAG_SYSTEM_PROXY != 0,
            weak: self.flags & MIGO_NETWORK_FLAG_WEAK != 0,
            signal_strength_dbm: self.signal_strength_dbm,
        }
        .checked()
    }
}

// No pointers: one layout on every target.
const _: () = assert!(size_of::<MigoNetworkReport>() == 24);
const _: () = assert!(std::mem::offset_of!(MigoNetworkReport, signal_strength_dbm) == 16);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MIGO_ABI_VERSION_CURRENT;

    fn report(network_type: u32, flags: u32, signal_strength_dbm: i32) -> MigoNetworkReport {
        MigoNetworkReport {
            header: VersionedHeader {
                struct_size: size_of::<MigoNetworkReport>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            network_type,
            flags,
            signal_strength_dbm,
            reserved0: 0,
        }
    }

    /// A report carries what the host knows, each flag as itself.
    #[test]
    fn a_report_carries_the_proxy_the_signal_and_the_host_s_judgement() {
        let wifi = report(
            1,
            MIGO_NETWORK_FLAG_CONNECTED | MIGO_NETWORK_FLAG_SYSTEM_PROXY,
            -58,
        );
        // SAFETY: a local record.
        let parsed = unsafe { MigoNetworkReport::parse(&wifi) }.expect("a Wi-Fi report");
        assert_eq!(
            parsed,
            ValidatedNetworkReport {
                network_type: 1,
                connected: true,
                system_proxy: true,
                weak: false,
                signal_strength_dbm: -58,
            }
        );
        assert_eq!(
            ValidatedNetworkReport::status(5, true),
            Ok(ValidatedNetworkReport {
                network_type: 5,
                connected: true,
                system_proxy: false,
                weak: false,
                signal_strength_dbm: 0,
            })
        );
    }

    /// What no device reports is refused: a type the header does not define, a flag
    /// it does not define, no network that is connected or weak, a positive dBm, a
    /// reserved field that is not zero, and a record shorter than the first version.
    #[test]
    fn what_no_device_reports_is_refused() {
        let refused = |report: MigoNetworkReport| {
            // SAFETY: a local record.
            unsafe { MigoNetworkReport::parse(&report) }.unwrap_err()
        };
        assert_eq!(refused(report(7, 0, 0)), MIGO_ERROR_INVALID_ARGUMENT);
        assert_eq!(refused(report(1, 1 << 3, 0)), MIGO_ERROR_INVALID_ARGUMENT);
        assert_eq!(
            refused(report(0, MIGO_NETWORK_FLAG_CONNECTED, 0)),
            MIGO_ERROR_INVALID_ARGUMENT
        );
        assert_eq!(
            refused(report(0, MIGO_NETWORK_FLAG_WEAK, 0)),
            MIGO_ERROR_INVALID_ARGUMENT
        );
        assert_eq!(refused(report(1, 0, 3)), MIGO_ERROR_INVALID_ARGUMENT);
        let mut reserved = report(1, 0, 0);
        reserved.reserved0 = 1;
        assert_eq!(refused(reserved), MIGO_ERROR_INVALID_ARGUMENT);
        let mut short = report(1, 0, 0);
        short.header.struct_size = 16;
        assert!(refused(short) != crate::MIGO_OK);
        assert!(ValidatedNetworkReport::status(0, true).is_err());
    }
}
