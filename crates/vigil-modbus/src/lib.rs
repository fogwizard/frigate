//! Modbus RTU master.
//!
//! Call [`open_bus`] once at startup and [`close_bus`] at shutdown. [`Bus`]
//! and [`write_feed`] reuse that port. [`write_feed`] waits 500 character
//! times for the slave (500 ms at 9600 baud). No reply is a failed write.
//! An empty port skips the serial open.

mod codec;
mod master;

pub use codec::{
    crc16, decode_response, request_read_coils, request_read_discrete_inputs,
    request_read_holding_registers, request_read_input_registers, request_write_multiple_coils,
    request_write_multiple_registers, request_write_single_coil, request_write_single_register,
    Error, Exception, Function, Response, Result,
};
pub use master::{
    close_bus, last_slave, open_bus, response_timeout, serial_link, write_feed, Bus,
};

/// Function codes this crate implements.
pub mod function {
    pub const READ_COILS: u8 = 0x01;
    pub const READ_DISCRETE_INPUTS: u8 = 0x02;
    pub const READ_HOLDING_REGISTERS: u8 = 0x03;
    pub const READ_INPUT_REGISTERS: u8 = 0x04;
    pub const WRITE_SINGLE_COIL: u8 = 0x05;
    pub const WRITE_SINGLE_REGISTER: u8 = 0x06;
    pub const WRITE_MULTIPLE_COILS: u8 = 0x0F;
    pub const WRITE_MULTIPLE_REGISTERS: u8 = 0x10;
}
