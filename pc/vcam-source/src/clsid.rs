//! CLSID of the ACAM virtual camera media source.

use windows::core::GUID;

/// Shared with `acam.exe`, which creates the camera from it.
pub use vcam_ipc::CLSID_STR;

pub const CLSID: GUID = GUID::from_u128(0xADD1E890_C3A2_4A7F_B104_76EDC7DF80AC);

#[cfg(test)]
mod tests {
    #[test]
    fn string_matches_guid() {
        assert_eq!(format!("{{{:?}}}", super::CLSID), super::CLSID_STR);
    }
}
