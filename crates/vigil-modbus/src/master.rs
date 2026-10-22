//! Serial RTU transport. One process-wide lock keeps frames from interleaving
//! when several feeders share the bus.

use crate::codec::{self, Error, Response};
use crate::function;
use serialport::SerialPort;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Duration;

struct OpenPort {
    name: String,
    io: Box<dyn SerialPort>,
}

static PORT: Mutex<Option<OpenPort>> = Mutex::new(None);
static LAST_SLAVE: AtomicU8 = AtomicU8::new(0);

/// Open the RTU port for the life of the process. An empty `port` leaves it closed.
/// A later call replaces the previous port.
pub fn open_bus(port: &str, baud: u32) -> Result<(), Error> {
    let name = port.trim();
    if name.is_empty() {
        tracing::info!("modbus serial skipped (port empty)");
        return Ok(());
    }
    let timeout = response_timeout(baud);
    let io = serialport::new(name, baud)
        .timeout(timeout)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .open()
        .map_err(|e| Error::Serial {
            port: name.to_string(),
            message: e.to_string(),
        })?;
    let mut slot = PORT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(prev) = slot.take() {
        tracing::info!(port = %prev.name, "modbus serial close");
    }
    tracing::info!(
        port = name,
        baud,
        timeout_ms = timeout.as_millis() as u64,
        "modbus serial open"
    );
    *slot = Some(OpenPort {
        name: name.to_string(),
        io,
    });
    Ok(())
}

/// Close the port opened by [`open_bus`]. Safe to call when it is already closed.
pub fn close_bus() {
    let mut slot = PORT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(prev) = slot.take() {
        tracing::info!(port = %prev.name, "modbus serial close");
    }
}

/// Slave address passed to the most recent [`write_feed`] in this process.
pub fn last_slave() -> u8 {
    LAST_SLAVE.load(Ordering::SeqCst)
}

/// Master calls on the port opened by [`open_bus`].
#[derive(Debug, Clone)]
pub struct Bus {
    timeout: Duration,
}

impl Bus {
    pub fn new(timeout: Duration) -> Self {
        let timeout = if timeout.is_zero() {
            Duration::from_millis(1)
        } else {
            timeout
        };
        Self { timeout }
    }

    pub fn read_coils(&self, slave: u8, address: u16, quantity: u16) -> Result<Vec<bool>, Error> {
        self.bits(function::READ_COILS, slave, address, quantity)
    }

    pub fn read_discrete_inputs(
        &self,
        slave: u8,
        address: u16,
        quantity: u16,
    ) -> Result<Vec<bool>, Error> {
        self.bits(function::READ_DISCRETE_INPUTS, slave, address, quantity)
    }

    pub fn read_holding_registers(
        &self,
        slave: u8,
        address: u16,
        quantity: u16,
    ) -> Result<Vec<u16>, Error> {
        self.registers(function::READ_HOLDING_REGISTERS, slave, address, quantity)
    }

    pub fn read_input_registers(
        &self,
        slave: u8,
        address: u16,
        quantity: u16,
    ) -> Result<Vec<u16>, Error> {
        self.registers(function::READ_INPUT_REGISTERS, slave, address, quantity)
    }

    pub fn write_single_coil(&self, slave: u8, address: u16, on: bool) -> Result<(), Error> {
        let frame = codec::request_write_single_coil(slave, address, on)?;
        match self.transact(&frame, slave, function::WRITE_SINGLE_COIL, address, 1)? {
            Response::Echo { value, .. } => {
                let echoed = value == 0xFF00;
                if echoed != on {
                    return Err(Error::Protocol(format!(
                        "coil echo {value:#06x} != requested {on}"
                    )));
                }
                Ok(())
            }
            other => Err(Error::Protocol(format!("unexpected coil response {other:?}"))),
        }
    }

    pub fn write_single_register(&self, slave: u8, address: u16, value: u16) -> Result<(), Error> {
        let frame = codec::request_write_single_register(slave, address, value)?;
        match self.transact(&frame, slave, function::WRITE_SINGLE_REGISTER, address, 1)? {
            Response::Echo { value: echoed, .. } if echoed == value => Ok(()),
            Response::Echo { value: echoed, .. } => Err(Error::Protocol(format!(
                "register echo {echoed} != {value}"
            ))),
            other => Err(Error::Protocol(format!(
                "unexpected register response {other:?}"
            ))),
        }
    }

    pub fn write_multiple_coils(
        &self,
        slave: u8,
        address: u16,
        coils: &[bool],
    ) -> Result<(), Error> {
        let frame = codec::request_write_multiple_coils(slave, address, coils)?;
        let quantity = coils.len() as u16;
        match self.transact(&frame, slave, function::WRITE_MULTIPLE_COILS, address, quantity)? {
            Response::Ack { .. } => Ok(()),
            other => Err(Error::Protocol(format!("unexpected coil ack {other:?}"))),
        }
    }

    pub fn write_multiple_registers(
        &self,
        slave: u8,
        address: u16,
        values: &[u16],
    ) -> Result<(), Error> {
        let frame = codec::request_write_multiple_registers(slave, address, values)?;
        let quantity = values.len() as u16;
        match self.transact(
            &frame,
            slave,
            function::WRITE_MULTIPLE_REGISTERS,
            address,
            quantity,
        )? {
            Response::Ack { .. } => Ok(()),
            other => Err(Error::Protocol(format!(
                "unexpected register ack {other:?}"
            ))),
        }
    }

    fn bits(
        &self,
        function: u8,
        slave: u8,
        address: u16,
        quantity: u16,
    ) -> Result<Vec<bool>, Error> {
        let frame = match function {
            function::READ_COILS => codec::request_read_coils(slave, address, quantity)?,
            _ => codec::request_read_discrete_inputs(slave, address, quantity)?,
        };
        match self.transact(&frame, slave, function, address, quantity)? {
            Response::Bits(bits) => Ok(bits),
            other => Err(Error::Protocol(format!("unexpected bit response {other:?}"))),
        }
    }

    fn registers(
        &self,
        function: u8,
        slave: u8,
        address: u16,
        quantity: u16,
    ) -> Result<Vec<u16>, Error> {
        let frame = match function {
            function::READ_HOLDING_REGISTERS => {
                codec::request_read_holding_registers(slave, address, quantity)?
            }
            _ => codec::request_read_input_registers(slave, address, quantity)?,
        };
        match self.transact(&frame, slave, function, address, quantity)? {
            Response::Registers(regs) => Ok(regs),
            other => Err(Error::Protocol(format!(
                "unexpected register response {other:?}"
            ))),
        }
    }

    fn transact(
        &self,
        request: &[u8],
        slave: u8,
        function: u8,
        address: u16,
        quantity: u16,
    ) -> Result<Response, Error> {
        let response = self.exchange(request, function)?;
        codec::decode_response(slave, function, address, quantity, &response)
    }

    /// Write one frame and read the matching response on the already-open port.
    pub fn exchange(&self, request: &[u8], function: u8) -> Result<Vec<u8>, Error> {
        if request.len() < 2 {
            return Err(Error::Protocol("request shorter than 2 bytes".into()));
        }
        let mut slot = PORT.lock().unwrap_or_else(|e| e.into_inner());
        let Some(open) = slot.as_mut() else {
            return Err(Error::Protocol("serial port is not open".into()));
        };
        let name = open.name.clone();
        let io_err = |e: std::io::Error| Error::Serial {
            port: name.clone(),
            message: e.to_string(),
        };
        open.io
            .set_timeout(self.timeout)
            .map_err(|e| Error::Serial {
                port: name.clone(),
                message: e.to_string(),
            })?;
        open.io
            .clear(serialport::ClearBuffer::Input)
            .map_err(|e| Error::Serial {
                port: name.clone(),
                message: e.to_string(),
            })?;
        open.io.write_all(request).map_err(io_err)?;
        open.io.flush().map_err(|e| Error::Serial {
            port: name,
            message: e.to_string(),
        })?;
        read_frame(&mut open.io, function)
    }
}

/// 500 character times. At 9600 baud one byte is about 1 ms, so the window is 500 ms.
pub fn response_timeout(baud: u32) -> Duration {
    let baud = u64::from(baud.max(1));
    Duration::from_millis((500 * 9600 / baud).max(1))
}

/// Write one feeder frame (function 0x05 or 0x06) and wait for the slave echo.
/// An empty `port` records `slave` and does not open a device.
pub fn write_feed(
    port: &str,
    baud: u32,
    slave: u8,
    function: u8,
    address: u16,
    value: u16,
) -> Result<(), Error> {
    LAST_SLAVE.store(slave, Ordering::SeqCst);
    if port.trim().is_empty() {
        tracing::info!(slave, "feed serial skipped (port empty)");
        return Ok(());
    }
    let timeout = response_timeout(baud);
    let bus = Bus::new(timeout);
    let result = match function {
        function::WRITE_SINGLE_COIL => bus.write_single_coil(slave, address, value != 0),
        function::WRITE_SINGLE_REGISTER => bus.write_single_register(slave, address, value),
        other => Err(Error::Protocol(format!(
            "unsupported feed function {other:#04x} (use 0x05 or 0x06)"
        ))),
    };
    if let Err(ref e) = result {
        tracing::error!(
            port,
            baud,
            slave,
            timeout_ms = timeout.as_millis() as u64,
            error = %e,
            "写入失败"
        );
    }
    result
}

fn read_frame(port: &mut impl Read, function: u8) -> Result<Vec<u8>, Error> {
    let mut buf = Vec::with_capacity(64);
    let mut tmp = [0u8; 256];
    loop {
        match port.read(&mut tmp) {
            Ok(0) => return Err(Error::Protocol("serial closed".into())),
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() > 256 {
                    return Err(Error::Protocol("response too long".into()));
                }
                if let Some(len) = codec::expected_response_len(function, &buf)? {
                    if buf.len() > len {
                        return Err(Error::Protocol(format!(
                            "trailing {} bytes after modbus frame",
                            buf.len() - len
                        )));
                    }
                    if buf.len() == len {
                        return Ok(buf);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                if buf.is_empty() {
                    return Err(Error::Protocol("modbus response timeout".into()));
                }
                return Err(Error::Protocol(format!(
                    "incomplete modbus response ({} bytes)",
                    buf.len()
                )));
            }
            Err(e) => return Err(Error::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_window_is_500_character_times() {
        assert_eq!(response_timeout(9600), Duration::from_millis(500));
        assert_eq!(response_timeout(19200), Duration::from_millis(250));
        assert_eq!(response_timeout(4800), Duration::from_millis(1000));
    }

    #[test]
    fn write_feed_empty_port_records_slave() {
        write_feed("", 9600, 3, 0x06, 0, 1).unwrap();
        assert_eq!(last_slave(), 3);
    }

    #[test]
    fn write_feed_does_not_open_a_port_itself() {
        let err = write_feed("COM1", 9600, 3, 0x06, 0, 1).unwrap_err();
        assert!(err.to_string().contains("not open"));
    }

    #[test]
    fn write_feed_rejects_read_function() {
        let err = write_feed("COM1", 9600, 3, 0x03, 0, 1).unwrap_err();
        assert!(matches!(err, Error::Protocol(_)));
    }
}
