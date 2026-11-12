use core::fmt;

pub const FRAMING_VERSION: u8 = 3;
pub const HOST_ADDRESS: u8 = 0;
/// Temporary source address used only by unassigned-node Commissioning replies.
pub const COMMISSIONING_ADDRESS: u8 = 254;
pub const BROADCAST_ADDRESS: u8 = 255;
pub const HEADER_BYTES: usize = 10;
pub const CRC_BYTES: usize = 4;
pub const MAX_PAYLOAD_BYTES: usize = 512;
pub const MAX_RAW_FRAME_BYTES: usize = HEADER_BYTES + MAX_PAYLOAD_BYTES + CRC_BYTES;
pub const MAX_COBS_BODY_BYTES: usize = MAX_RAW_FRAME_BYTES + MAX_RAW_FRAME_BYTES.div_ceil(254);
pub const MAX_WIRE_FRAME_BYTES: usize = MAX_COBS_BODY_BYTES + 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FrameRole {
    Request = 1,
    Response = 2,
}

impl FrameRole {
    fn from_wire(value: u8) -> Result<Self, FrameError> {
        match value {
            1 => Ok(Self::Request),
            2 => Ok(Self::Response),
            _ => Err(FrameError::InvalidRole(value)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub role: FrameRole,
    pub destination: u8,
    pub source: u8,
    pub service: u8,
    pub message: u8,
    pub transaction_id: u32,
    payload: [u8; MAX_PAYLOAD_BYTES],
    payload_len: usize,
}

impl Frame {
    pub fn new(
        role: FrameRole,
        destination: u8,
        source: u8,
        service: u8,
        message: u8,
        transaction_id: u32,
        payload: &[u8],
    ) -> Result<Self, FrameError> {
        validate_route(role, destination, source)?;
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(FrameError::PayloadTooLarge);
        }
        let mut owned_payload = [0; MAX_PAYLOAD_BYTES];
        owned_payload[..payload.len()].copy_from_slice(payload);
        Ok(Self {
            role,
            destination,
            source,
            service,
            message,
            transaction_id,
            payload: owned_payload,
            payload_len: payload.len(),
        })
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_len]
    }

    pub fn encode(&self, output: &mut [u8; MAX_WIRE_FRAME_BYTES]) -> usize {
        let mut raw = [0; MAX_RAW_FRAME_BYTES];
        raw[0] = FRAMING_VERSION;
        raw[1] = self.role as u8;
        raw[2] = self.destination;
        raw[3] = self.source;
        raw[4] = self.service;
        raw[5] = self.message;
        raw[6..10].copy_from_slice(&self.transaction_id.to_le_bytes());
        raw[HEADER_BYTES..HEADER_BYTES + self.payload_len].copy_from_slice(self.payload());
        let crc_offset = HEADER_BYTES + self.payload_len;
        let crc = crc32c(&raw[..crc_offset]);
        raw[crc_offset..crc_offset + CRC_BYTES].copy_from_slice(&crc.to_le_bytes());

        let encoded_len = cobs_encode(&raw[..crc_offset + CRC_BYTES], output);
        output[encoded_len] = 0;
        encoded_len + 1
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    PayloadTooLarge,
    EncodedFrameTooLarge,
    MalformedCobs,
    FrameTooShort,
    UnsupportedVersion(u8),
    InvalidRole(u8),
    InvalidRoute,
    CrcMismatch,
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadTooLarge => formatter.write_str("payload exceeds 512 bytes"),
            Self::EncodedFrameTooLarge => {
                formatter.write_str("encoded frame exceeds bounded buffer")
            }
            Self::MalformedCobs => formatter.write_str("malformed COBS body"),
            Self::FrameTooShort => {
                formatter.write_str("decoded frame is shorter than header and CRC")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported framing version {version}")
            }
            Self::InvalidRole(role) => write!(formatter, "invalid frame role {role}"),
            Self::InvalidRoute => formatter.write_str("role and addresses form an invalid route"),
            Self::CrcMismatch => formatter.write_str("frame CRC-32C mismatch"),
        }
    }
}

impl std::error::Error for FrameError {}

pub struct StreamDecoder {
    encoded: [u8; MAX_COBS_BODY_BYTES],
    encoded_len: usize,
    discarding: bool,
}

impl StreamDecoder {
    pub const fn new() -> Self {
        Self {
            encoded: [0; MAX_COBS_BODY_BYTES],
            encoded_len: 0,
            discarding: false,
        }
    }

    pub fn push(&mut self, byte: u8) -> Option<Result<Frame, FrameError>> {
        if byte != 0 {
            if self.discarding {
                return None;
            }
            if self.encoded_len == self.encoded.len() {
                self.encoded_len = 0;
                self.discarding = true;
                return None;
            }
            self.encoded[self.encoded_len] = byte;
            self.encoded_len += 1;
            return None;
        }

        if self.discarding {
            self.discarding = false;
            return Some(Err(FrameError::EncodedFrameTooLarge));
        }
        if self.encoded_len == 0 {
            return None;
        }

        let result = decode_body(&self.encoded[..self.encoded_len]);
        self.encoded_len = 0;
        Some(result)
    }

    pub fn discard_partial(&mut self) {
        self.encoded_len = 0;
        self.discarding = false;
    }
}

impl Default for StreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

pub fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

fn validate_route(role: FrameRole, destination: u8, source: u8) -> Result<(), FrameError> {
    let valid = match role {
        FrameRole::Request => source == HOST_ADDRESS && destination != HOST_ADDRESS,
        FrameRole::Response => {
            destination == HOST_ADDRESS && source != HOST_ADDRESS && source != BROADCAST_ADDRESS
        }
    };
    if valid {
        Ok(())
    } else {
        Err(FrameError::InvalidRoute)
    }
}

fn decode_body(encoded: &[u8]) -> Result<Frame, FrameError> {
    let mut raw = [0; MAX_RAW_FRAME_BYTES];
    let raw_len = cobs_decode(encoded, &mut raw)?;
    if raw_len < HEADER_BYTES + CRC_BYTES {
        return Err(FrameError::FrameTooShort);
    }
    if raw[0] != FRAMING_VERSION {
        return Err(FrameError::UnsupportedVersion(raw[0]));
    }
    let role = FrameRole::from_wire(raw[1])?;
    validate_route(role, raw[2], raw[3])?;

    let crc_offset = raw_len - CRC_BYTES;
    let expected_crc = u32::from_le_bytes(raw[crc_offset..raw_len].try_into().unwrap());
    if crc32c(&raw[..crc_offset]) != expected_crc {
        return Err(FrameError::CrcMismatch);
    }
    let transaction_id = u32::from_le_bytes(raw[6..10].try_into().unwrap());
    Frame::new(
        role,
        raw[2],
        raw[3],
        raw[4],
        raw[5],
        transaction_id,
        &raw[HEADER_BYTES..crc_offset],
    )
}

fn cobs_encode(input: &[u8], output: &mut [u8]) -> usize {
    let mut code_index = 0;
    let mut write_index = 1;
    let mut code = 1u8;

    for &byte in input {
        if byte == 0 {
            output[code_index] = code;
            code_index = write_index;
            write_index += 1;
            code = 1;
        } else {
            output[write_index] = byte;
            write_index += 1;
            code += 1;
            if code == 0xff {
                output[code_index] = code;
                code_index = write_index;
                write_index += 1;
                code = 1;
            }
        }
    }
    output[code_index] = code;
    write_index
}

fn cobs_decode(input: &[u8], output: &mut [u8; MAX_RAW_FRAME_BYTES]) -> Result<usize, FrameError> {
    let mut read_index = 0;
    let mut write_index = 0;
    while read_index < input.len() {
        let code = input[read_index];
        if code == 0 {
            return Err(FrameError::MalformedCobs);
        }
        read_index += 1;
        let count = usize::from(code) - 1;
        if read_index + count > input.len() || write_index + count > output.len() {
            return Err(FrameError::MalformedCobs);
        }
        output[write_index..write_index + count]
            .copy_from_slice(&input[read_index..read_index + count]);
        read_index += count;
        write_index += count;
        if code != 0xff && read_index < input.len() {
            if write_index == output.len() {
                return Err(FrameError::MalformedCobs);
            }
            output[write_index] = 0;
            write_index += 1;
        }
    }
    Ok(write_index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(payload: &[u8]) -> Frame {
        Frame::new(FrameRole::Request, 7, 0, 2, 3, 0x1234_5678, payload).unwrap()
    }

    fn wire(frame: &Frame) -> Vec<u8> {
        let mut bytes = [0; MAX_WIRE_FRAME_BYTES];
        let len = frame.encode(&mut bytes);
        bytes[..len].to_vec()
    }

    fn decode(bytes: &[u8]) -> Result<Frame, FrameError> {
        let mut decoder = StreamDecoder::new();
        let mut result = None;
        for &byte in bytes {
            if let Some(event) = decoder.push(byte) {
                result = Some(event);
            }
        }
        result.expect("wire frame must end with an event")
    }

    #[test]
    fn crc32c_matches_standard_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }

    #[test]
    fn round_trips_zeroes_and_maximum_payload() {
        let payload: Vec<u8> = (0..MAX_PAYLOAD_BYTES).map(|i| (i % 256) as u8).collect();
        let frame = request(&payload);
        let encoded = wire(&frame);
        assert!(encoded.len() <= MAX_WIRE_FRAME_BYTES);
        assert_eq!(decode(&encoded).unwrap(), frame);
    }

    #[test]
    fn accepts_every_possible_input_fragmentation() {
        let expected = request(&[0, 1, 2, 0, 4]);
        let encoded = wire(&expected);
        for split in 0..=encoded.len() {
            let mut decoder = StreamDecoder::new();
            let mut result = None;
            for chunk in [&encoded[..split], &encoded[split..]] {
                for &byte in chunk {
                    if let Some(event) = decoder.push(byte) {
                        result = Some(event);
                    }
                }
            }
            assert_eq!(result.unwrap().unwrap(), expected);
        }
    }

    #[test]
    fn rejects_every_single_bit_corruption_of_the_body() {
        let encoded = wire(&request(&[1, 2, 3, 4, 5]));
        for index in 0..encoded.len() - 1 {
            for bit in 0..8 {
                let mut damaged = encoded.clone();
                damaged[index] ^= 1 << bit;
                assert!(decode(&damaged).is_err(), "index={index}, bit={bit}");
            }
        }
    }

    #[test]
    fn emits_multiple_frames_from_one_stream() {
        let first = request(&[1, 2, 3]);
        let second = request(&[4, 0, 5]);
        let stream = [wire(&first), wire(&second)].concat();
        let mut decoder = StreamDecoder::new();
        let events: Vec<_> = stream
            .into_iter()
            .filter_map(|byte| decoder.push(byte))
            .collect();
        assert_eq!(events, vec![Ok(first), Ok(second)]);
    }

    #[test]
    fn deletion_or_insertion_cannot_create_an_accepted_original_frame() {
        let expected = request(&[1, 0, 2, 3, 4, 5]);
        let encoded = wire(&expected);
        for index in 0..encoded.len() - 1 {
            let mut deleted = encoded.clone();
            deleted.remove(index);
            assert_ne!(
                decode(&deleted),
                Ok(expected.clone()),
                "deleted index={index}"
            );

            let mut inserted = encoded.clone();
            inserted.insert(index, 0x55);
            assert_ne!(
                decode(&inserted),
                Ok(expected.clone()),
                "inserted index={index}"
            );
        }
    }

    #[test]
    fn lost_delimiter_drops_joined_frames_then_recovers() {
        let frame = request(&[4, 5, 6]);
        let mut without_delimiter = wire(&frame);
        without_delimiter.pop();
        let mut stream = without_delimiter.clone();
        stream.extend(wire(&frame));
        stream.extend(wire(&frame));

        let mut decoder = StreamDecoder::new();
        let events: Vec<_> = stream
            .into_iter()
            .filter_map(|byte| decoder.push(byte))
            .collect();
        assert!(events[0].is_err());
        assert_eq!(events[1], Ok(frame));
    }

    #[test]
    fn recovers_after_malformed_and_oversized_candidates() {
        let good = request(&[9, 8, 7]);
        let good_wire = wire(&good);
        let mut decoder = StreamDecoder::new();
        for _ in 0..=MAX_COBS_BODY_BYTES {
            assert!(decoder.push(1).is_none());
        }
        assert_eq!(decoder.push(0), Some(Err(FrameError::EncodedFrameTooLarge)));
        let mut recovered = None;
        for byte in good_wire {
            if let Some(event) = decoder.push(byte) {
                recovered = Some(event);
            }
        }
        assert_eq!(recovered.unwrap().unwrap(), good);
    }

    #[test]
    fn enforces_host_initiated_routes_and_payload_limit() {
        assert_eq!(
            Frame::new(FrameRole::Request, 0, 7, 1, 1, 1, &[]),
            Err(FrameError::InvalidRoute)
        );
        assert_eq!(
            Frame::new(FrameRole::Response, 0, 255, 1, 1, 1, &[]),
            Err(FrameError::InvalidRoute)
        );
        assert_eq!(
            Frame::new(
                FrameRole::Request,
                7,
                0,
                1,
                1,
                1,
                &[0; MAX_PAYLOAD_BYTES + 1]
            ),
            Err(FrameError::PayloadTooLarge)
        );
    }

    #[test]
    fn matches_shared_golden_vectors() {
        let empty = request(&[]);
        assert_eq!(
            wire(&empty),
            include_bytes!("../../../contracts/golden/framing_v3_request_empty.bin")
        );

        let response = Frame::new(
            FrameRole::Response,
            0,
            7,
            2,
            4,
            0x89ab_cdef,
            &[0, 1, 0, 255, 16],
        )
        .unwrap();
        assert_eq!(
            wire(&response),
            include_bytes!("../../../contracts/golden/framing_v3_response_mixed.bin")
        );

        let payload: Vec<u8> = (0..MAX_PAYLOAD_BYTES).map(|i| i as u8).collect();
        assert_eq!(
            wire(&request(&payload)),
            include_bytes!("../../../contracts/golden/framing_v3_request_max.bin")
        );
    }
}
