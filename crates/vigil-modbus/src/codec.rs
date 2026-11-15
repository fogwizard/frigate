//! Modbus RTU request encoder and response decoder.

use crate::function;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("modbus exception function {function:#04x} code {code:#04x} ({name})")]
    Exception {
        function: u8,
        code: u8,
        name: &'static str,
    },
    #[error("modbus crc mismatch")]
    BadCrc,
    #[error("modbus frame truncated")]
    Truncated,
    #[error("modbus slave mismatch: got {got}, expected {expected}")]
    SlaveMismatch { got: u8, expected: u8 },
    #[error("modbus function mismatch: got {got:#04x}, expected {expected:#04x}")]
    FunctionMismatch { got: u8, expected: u8 },
    #[error("{0}")]
    Protocol(String),
    #[error("serial {port}: {message}")]
    Serial { port: String, message: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Function codes supported by the master.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Function {
    ReadCoils = function::READ_COILS,
    ReadDiscreteInputs = function::READ_DISCRETE_INPUTS,
    ReadHoldingRegisters = function::READ_HOLDING_REGISTERS,
    ReadInputRegisters = function::READ_INPUT_REGISTERS,
    WriteSingleCoil = function::WRITE_SINGLE_COIL,
    WriteSingleRegister = function::WRITE_SINGLE_REGISTER,
    WriteMultipleCoils = function::WRITE_MULTIPLE_COILS,
    WriteMultipleRegisters = function::WRITE_MULTIPLE_REGISTERS,
}

impl Function {
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Result<Self> {
        Ok(match code {
            function::READ_COILS => Self::ReadCoils,
            function::READ_DISCRETE_INPUTS => Self::ReadDiscreteInputs,
            function::READ_HOLDING_REGISTERS => Self::ReadHoldingRegisters,
            function::READ_INPUT_REGISTERS => Self::ReadInputRegisters,
            function::WRITE_SINGLE_COIL => Self::WriteSingleCoil,
            function::WRITE_SINGLE_REGISTER => Self::WriteSingleRegister,
            function::WRITE_MULTIPLE_COILS => Self::WriteMultipleCoils,
            function::WRITE_MULTIPLE_REGISTERS => Self::WriteMultipleRegisters,
            other => {
                return Err(Error::Protocol(format!(
                    "unsupported modbus function {other:#04x}"
                )))
            }
        })
    }
}

/// A decoded slave response. Exception frames are returned as [`Error::Exception`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// Coils or discrete inputs, in address order.
    Bits(Vec<bool>),
    /// Holding or input registers, big-endian on the wire.
    Registers(Vec<u16>),
    /// Echo of a write-single-coil or write-single-register request.
    Echo { address: u16, value: u16 },
    /// Echo of a write-multiple request: start address and quantity.
    Ack { address: u16, quantity: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exception {
    pub function: u8,
    pub code: u8,
}

impl Exception {
    pub fn name(self) -> &'static str {
        match self.code {
            0x01 => "illegal function",
            0x02 => "illegal data address",
            0x03 => "illegal data value",
            0x04 => "server device failure",
            0x05 => "acknowledge",
            0x06 => "server device busy",
            0x08 => "memory parity error",
            0x0A => "gateway path unavailable",
            0x0B => "gateway target device failed to respond",
            _ => "unknown",
        }
    }
}

/// CRC-16/MODBUS (poly 0xA001, init 0xFFFF). The low byte is sent first.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

pub fn request_read_coils(slave: u8, address: u16, quantity: u16) -> Result<Vec<u8>> {
    read_request(slave, function::READ_COILS, address, quantity, 2000)
}

pub fn request_read_discrete_inputs(slave: u8, address: u16, quantity: u16) -> Result<Vec<u8>> {
    read_request(slave, function::READ_DISCRETE_INPUTS, address, quantity, 2000)
}

pub fn request_read_holding_registers(slave: u8, address: u16, quantity: u16) -> Result<Vec<u8>> {
    read_request(
        slave,
        function::READ_HOLDING_REGISTERS,
        address,
        quantity,
        125,
    )
}

pub fn request_read_input_registers(slave: u8, address: u16, quantity: u16) -> Result<Vec<u8>> {
    read_request(slave, function::READ_INPUT_REGISTERS, address, quantity, 125)
}

pub fn request_write_single_coil(slave: u8, address: u16, on: bool) -> Result<Vec<u8>> {
    let value = if on { 0xFF00 } else { 0x0000 };
    Ok(with_crc(write_single_body(
        slave,
        function::WRITE_SINGLE_COIL,
        address,
        value,
    )))
}

pub fn request_write_single_register(slave: u8, address: u16, value: u16) -> Result<Vec<u8>> {
    Ok(with_crc(write_single_body(
        slave,
        function::WRITE_SINGLE_REGISTER,
        address,
        value,
    )))
}

pub fn request_write_multiple_coils(slave: u8, address: u16, coils: &[bool]) -> Result<Vec<u8>> {
    let quantity = u16_len(coils.len(), 1968)?;
    check_span(address, quantity)?;
    let packed = pack_bits(coils);
    let mut body = write_multi_head(slave, function::WRITE_MULTIPLE_COILS, address, quantity);
    body.push(packed.len() as u8);
    body.extend(packed);
    Ok(with_crc(body))
}

pub fn request_write_multiple_registers(
    slave: u8,
    address: u16,
    values: &[u16],
) -> Result<Vec<u8>> {
    let quantity = u16_len(values.len(), 123)?;
    check_span(address, quantity)?;
    let mut body = write_multi_head(
        slave,
        function::WRITE_MULTIPLE_REGISTERS,
        address,
        quantity,
    );
    body.push((values.len() * 2) as u8);
    for value in values {
        body.push((value >> 8) as u8);
        body.push((value & 0xFF) as u8);
    }
    Ok(with_crc(body))
}

/// Decode a complete RTU frame, including CRC.
///
/// `quantity` is the quantity from the request. Read responses do not repeat it,
/// and the unused high bits of the last coil byte are ignored.
pub fn decode_response(
    slave: u8,
    function: u8,
    address: u16,
    quantity: u16,
    frame: &[u8],
) -> Result<Response> {
    if frame.len() < 5 {
        return Err(Error::Truncated);
    }
    check_crc(frame)?;
    if frame[0] != slave {
        return Err(Error::SlaveMismatch {
            got: frame[0],
            expected: slave,
        });
    }
    if frame[1] == function | 0x80 {
        let code = frame[2];
        let exc = Exception { function, code };
        return Err(Error::Exception {
            function,
            code,
            name: exc.name(),
        });
    }
    if frame[1] != function {
        return Err(Error::FunctionMismatch {
            got: frame[1],
            expected: function,
        });
    }
    match function {
        function::READ_COILS | function::READ_DISCRETE_INPUTS => {
            let data = read_data(frame, (quantity as usize + 7) / 8)?;
            Ok(Response::Bits(unpack_bits(data, quantity)?))
        }
        function::READ_HOLDING_REGISTERS | function::READ_INPUT_REGISTERS => {
            let data = read_data(frame, quantity as usize * 2)?;
            Ok(Response::Registers(unpack_registers(data)?))
        }
        function::WRITE_SINGLE_COIL | function::WRITE_SINGLE_REGISTER => {
            if frame.len() != 8 {
                return Err(Error::Protocol(format!(
                    "write-single response length {}",
                    frame.len()
                )));
            }
            let got_addr = u16::from_be_bytes([frame[2], frame[3]]);
            if got_addr != address {
                return Err(Error::Protocol(format!(
                    "write echo address {got_addr} != {address}"
                )));
            }
            Ok(Response::Echo {
                address: got_addr,
                value: u16::from_be_bytes([frame[4], frame[5]]),
            })
        }
        function::WRITE_MULTIPLE_COILS | function::WRITE_MULTIPLE_REGISTERS => {
            if frame.len() != 8 {
                return Err(Error::Protocol(format!(
                    "write-multiple response length {}",
                    frame.len()
                )));
            }
            let got_addr = u16::from_be_bytes([frame[2], frame[3]]);
            let got_qty = u16::from_be_bytes([frame[4], frame[5]]);
            if got_addr != address || got_qty != quantity {
                return Err(Error::Protocol(format!(
                    "write ack {got_addr}/{got_qty} != {address}/{quantity}"
                )));
            }
            Ok(Response::Ack {
                address: got_addr,
                quantity: got_qty,
            })
        }
        other => Err(Error::Protocol(format!(
            "unsupported modbus function {other:#04x}"
        ))),
    }
}

/// How many bytes a response to `function` should be, once enough of `buf` is known.
/// `None` means keep reading.
pub(crate) fn expected_response_len(function: u8, buf: &[u8]) -> Result<Option<usize>> {
    if buf.len() < 2 {
        return Ok(None);
    }
    if buf[1] == function | 0x80 {
        return Ok(Some(5));
    }
    if buf[1] != function {
        return Err(Error::FunctionMismatch {
            got: buf[1],
            expected: function,
        });
    }
    match function {
        function::READ_COILS
        | function::READ_DISCRETE_INPUTS
        | function::READ_HOLDING_REGISTERS
        | function::READ_INPUT_REGISTERS => {
            if buf.len() < 3 {
                return Ok(None);
            }
            let len = 3 + buf[2] as usize + 2;
            if len > 256 {
                return Err(Error::Protocol(format!("response length {len}")));
            }
            Ok(Some(len))
        }
        function::WRITE_SINGLE_COIL
        | function::WRITE_SINGLE_REGISTER
        | function::WRITE_MULTIPLE_COILS
        | function::WRITE_MULTIPLE_REGISTERS => Ok(Some(8)),
        other => Err(Error::Protocol(format!(
            "unsupported modbus function {other:#04x}"
        ))),
    }
}

fn read_request(slave: u8, function: u8, address: u16, quantity: u16, max: u16) -> Result<Vec<u8>> {
    check_quantity(quantity, max)?;
    check_span(address, quantity)?;
    Ok(with_crc(vec![
        slave,
        function,
        (address >> 8) as u8,
        (address & 0xFF) as u8,
        (quantity >> 8) as u8,
        (quantity & 0xFF) as u8,
    ]))
}

fn write_single_body(slave: u8, function: u8, address: u16, value: u16) -> Vec<u8> {
    vec![
        slave,
        function,
        (address >> 8) as u8,
        (address & 0xFF) as u8,
        (value >> 8) as u8,
        (value & 0xFF) as u8,
    ]
}

fn write_multi_head(slave: u8, function: u8, address: u16, quantity: u16) -> Vec<u8> {
    vec![
        slave,
        function,
        (address >> 8) as u8,
        (address & 0xFF) as u8,
        (quantity >> 8) as u8,
        (quantity & 0xFF) as u8,
    ]
}

fn with_crc(mut body: Vec<u8>) -> Vec<u8> {
    let crc = crc16(&body);
    body.push((crc & 0xFF) as u8);
    body.push((crc >> 8) as u8);
    body
}

fn check_crc(frame: &[u8]) -> Result<()> {
    if frame.len() < 4 {
        return Err(Error::Truncated);
    }
    let (body, tail) = frame.split_at(frame.len() - 2);
    let expect = crc16(body);
    let got = u16::from(tail[0]) | (u16::from(tail[1]) << 8);
    if got != expect {
        return Err(Error::BadCrc);
    }
    Ok(())
}

fn check_quantity(quantity: u16, max: u16) -> Result<()> {
    if quantity == 0 || quantity > max {
        return Err(Error::Protocol(format!(
            "quantity {quantity} out of range 1..={max}"
        )));
    }
    Ok(())
}

fn check_span(address: u16, quantity: u16) -> Result<()> {
    if u32::from(address) + u32::from(quantity) > 0x1_0000 {
        return Err(Error::Protocol(format!(
            "address {address} + quantity {quantity} exceeds 65536"
        )));
    }
    Ok(())
}

fn u16_len(len: usize, max: u16) -> Result<u16> {
    if len == 0 || len > usize::from(max) {
        return Err(Error::Protocol(format!(
            "quantity {len} out of range 1..={max}"
        )));
    }
    Ok(len as u16)
}

fn read_data(frame: &[u8], expect: usize) -> Result<&[u8]> {
    if frame.len() < 3 {
        return Err(Error::Truncated);
    }
    let count = frame[2] as usize;
    if count != expect || frame.len() != 3 + count + 2 {
        return Err(Error::Protocol(format!(
            "byte count {count}, expected {expect}"
        )));
    }
    Ok(&frame[3..3 + count])
}

fn unpack_registers(data: &[u8]) -> Result<Vec<u16>> {
    if data.len() % 2 != 0 {
        return Err(Error::Protocol("odd register payload".into()));
    }
    Ok(data
        .chunks(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect())
}

fn pack_bits(coils: &[bool]) -> Vec<u8> {
    let mut packed = vec![0u8; (coils.len() + 7) / 8];
    for (i, on) in coils.iter().enumerate() {
        if *on {
            packed[i / 8] |= 1 << (i % 8);
        }
    }
    packed
}

fn unpack_bits(bytes: &[u8], quantity: u16) -> Result<Vec<bool>> {
    let need = (quantity as usize + 7) / 8;
    if bytes.len() != need {
        return Err(Error::Protocol(format!(
            "coil bytes {}, expected {need}",
            bytes.len()
        )));
    }
    let mut out = Vec::with_capacity(quantity as usize);
    for i in 0..quantity as usize {
        out.push(bytes[i / 8] & (1 << (i % 8)) != 0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish(body: &[u8]) -> Vec<u8> {
        with_crc(body.to_vec())
    }

    #[test]
    fn crc_matches_known_read_request() {
        let crc = crc16(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x0A]);
        assert_eq!(crc, 0xCDC5);
        assert_eq!([(crc & 0xFF) as u8, (crc >> 8) as u8], [0xC5, 0xCD]);
    }

    #[test]
    fn read_holding_request_matches_known_frame() {
        let frame = request_read_holding_registers(1, 0, 10).unwrap();
        assert_eq!(frame, [0x01, 0x03, 0x00, 0x00, 0x00, 0x0A, 0xC5, 0xCD]);
    }

    #[test]
    fn write_register_and_coil_frames() {
        let reg = request_write_single_register(1, 0, 1).unwrap();
        assert_eq!(&reg[..6], &[0x01, 0x06, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(reg.len(), 8);
        let coil = request_write_single_coil(1, 0, true).unwrap();
        assert_eq!(&coil[..6], &[0x01, 0x05, 0x00, 0x00, 0xFF, 0x00]);
        let off = request_write_single_coil(2, 4, false).unwrap();
        assert_eq!(&off[..6], &[0x02, 0x05, 0x00, 0x04, 0x00, 0x00]);
    }

    #[test]
    fn write_multiple_registers_packs_big_endian() {
        let frame = request_write_multiple_registers(1, 0x0010, &[0x000A, 0x0102]).unwrap();
        assert_eq!(
            &frame[..11],
            &[0x01, 0x10, 0x00, 0x10, 0x00, 0x02, 0x04, 0x00, 0x0A, 0x01, 0x02]
        );
        assert_eq!(frame.len(), 13);
        let ack = finish(&[0x01, 0x10, 0x00, 0x10, 0x00, 0x02]);
        assert_eq!(
            decode_response(1, 0x10, 0x0010, 2, &ack).unwrap(),
            Response::Ack {
                address: 0x0010,
                quantity: 2
            }
        );
    }

    #[test]
    fn write_multiple_coils_packs_lsb_first() {
        let coils = [true, false, true, true, false, false, false, true, true];
        let frame = request_write_multiple_coils(1, 0, &coils).unwrap();
        assert_eq!(&frame[..9], &[0x01, 0x0F, 0x00, 0x00, 0x00, 0x09, 0x02, 0x8D, 0x01]);
    }

    #[test]
    fn decode_registers_and_bits() {
        let regs = finish(&[0x01, 0x03, 0x04, 0x00, 0x0A, 0x00, 0x64]);
        assert_eq!(
            decode_response(1, 0x03, 0, 2, &regs).unwrap(),
            Response::Registers(vec![10, 100])
        );
        let bits = finish(&[0x01, 0x01, 0x02, 0x8D, 0x01]);
        assert_eq!(
            decode_response(1, 0x01, 0, 9, &bits).unwrap(),
            Response::Bits(vec![true, false, true, true, false, false, false, true, true])
        );
    }

    #[test]
    fn exception_and_bad_crc() {
        let frame = finish(&[0x01, 0x83, 0x02]);
        let err = decode_response(1, 0x03, 0, 1, &frame).unwrap_err();
        assert!(matches!(
            err,
            Error::Exception {
                function: 0x03,
                code: 0x02,
                name: "illegal data address"
            }
        ));
        let mut bad = request_read_holding_registers(1, 0, 1).unwrap();
        bad[6] ^= 0xFF;
        assert!(matches!(
            decode_response(1, 0x03, 0, 1, &bad),
            Err(Error::BadCrc)
        ));
    }

    #[test]
    fn quantity_limits() {
        assert!(request_read_holding_registers(1, 0, 0).is_err());
        assert!(request_read_holding_registers(1, 0, 126).is_err());
        assert!(request_read_coils(1, 0, 2001).is_err());
        assert!(request_write_multiple_registers(1, 0, &[]).is_err());
        assert!(request_read_holding_registers(1, 0xFFFF, 2).is_err());
        assert!(request_read_input_registers(1, 0, 1).is_ok());
        assert!(request_read_discrete_inputs(1, 0, 1).is_ok());
    }
}
