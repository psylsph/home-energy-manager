//! Inverter data model and control logic.
//!
//! Houses the inverter data model, register decoding/encoding,
//! periodic polling, and network discovery of GivEnergy inverters.

pub mod decoder;
pub mod discovery;
pub mod encoder;
pub mod model;
pub mod poll;
pub mod sanitizer;
pub mod state_machines;
