//! Fixed development CLSID for the ACAM test-pattern media source (Milestone 1).
//!
//! This is a hand-picked GUID, not a properly minted one — fine for local development, but if this
//! DLL ever ships to users it should get a freshly generated CLSID to avoid any collision risk.

use windows::core::GUID;

pub const CLSID_STR: &str = "{ACA40001-0000-4000-8000-000000000001}";

pub const CLSID: GUID = GUID::from_u128(0xACA40001_0000_4000_8000_000000000001);
