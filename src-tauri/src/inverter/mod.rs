//! Inverter data model and control logic.
//!
//! Houses the inverter data model, register decoding/encoding,
//! periodic polling, and network discovery of GivEnergy inverters.

pub(crate) mod agile;
pub(crate) mod auto_discovery;
pub(crate) mod cosy;
pub mod daily_report;
pub mod decoder;
pub mod discovery;
pub mod encoder;
pub(crate) mod forecast_plan;
pub mod model;
pub mod poll;
pub mod power_limit;
pub mod reconnect;
pub mod sanitizer;
pub(crate) mod solar_position;
pub mod state_machines;
