//! Backend implementations for bt_mon.
//!
//! This module contains the backend-specific implementations of the
//! `DeviceMonitor` and `GattClient` traits.

#[cfg(feature = "btleplug")]
pub mod btleplug;

#[cfg(feature = "bluer")]
pub mod bluer;

#[cfg(feature = "btleplug")]
pub use btleplug::BtleplugMonitor;

#[cfg(feature = "bluer")]
pub use bluer::BluerMonitor;
