//! The link layer of wired M-Bus, EN 13757-2: the three frames on the
//! line, and what tells them apart.
//!
//! A single character, `E5`, is the acknowledgement. A short frame opens
//! with `10`, carries a control and an address and closes with a checksum
//! and `16`. A long frame opens with `68`, its length twice, `68` again,
//! then control, address, control information and the user data, and
//! closes the same way. The length counts control, address, control
//! information and data, and is one byte — so a long frame carries at most
//! 252 bytes of data, which is why a Stream crosses in records over as
//! many telegrams as it takes. The checksum is the arithmetic sum of what
//! the length counts, modulo 256.

use transport::error::{Result, protocol_error};

/// The single-character acknowledgement.
pub const ACK: u8 = 0xe5;
/// What a short frame opens with.
pub const SHORT: u8 = 0x10;
/// What a long frame opens with, twice.
pub const LONG: u8 = 0x68;
/// What every frame but the acknowledgement closes with.
pub const STOP: u8 = 0x16;
/// The most user data a long frame carries: 255 less control, address and
/// control information.
pub const MAX_USER_DATA: usize = 252;

/// Initialisation of the slave; answered with an acknowledgement.
pub const SND_NKE: u8 = 0x40;
/// User data to the slave; answered with an acknowledgement.
pub const SND_UD: u8 = 0x53;
/// A request for class 2 data; answered with `RSP_UD`.
pub const REQ_UD2: u8 = 0x5b;
/// The slave's data, in a long frame.
pub const RSP_UD: u8 = 0x08;
/// The frame count bit a master toggles between calls.
pub const FCB: u8 = 0x20;

/// Every slave answers; for one slave on the line.
pub const BROADCAST_REPLY: u8 = 0xfe;
/// Every slave takes it; none answers.
pub const BROADCAST: u8 = 0xff;

/// One frame on the line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Ack,
    Short {
        control: u8,
        address: u8,
    },
    Long {
        control: u8,
        address: u8,
        /// The control information: what the user data is.
        ci: u8,
        data: Vec<u8>,
    },
}

impl Frame {
    /// The bytes on the line.
    ///
    /// # Errors
    /// A long frame with more than [`MAX_USER_DATA`] bytes of data.
    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(match self {
            Self::Ack => vec![ACK],
            Self::Short { control, address } => {
                vec![
                    SHORT,
                    *control,
                    *address,
                    control.wrapping_add(*address),
                    STOP,
                ]
            }
            Self::Long {
                control,
                address,
                ci,
                data,
            } => {
                if data.len() > MAX_USER_DATA {
                    return Err(protocol_error(format!(
                        "{} bytes of user data is over a long frame's {MAX_USER_DATA}",
                        data.len()
                    )));
                }
                let length = u8::try_from(3 + data.len()).unwrap_or(u8::MAX);
                let mut out = vec![LONG, length, length, LONG, *control, *address, *ci];
                out.extend_from_slice(data);
                out.push(checksum(&out[4..]));
                out.push(STOP);
                out
            }
        })
    }

    /// The frame `bytes` carry, whole.
    ///
    /// # Errors
    /// A start byte that is none of the three, a length that does not match,
    /// a checksum that does not add up, or a missing stop.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        match bytes {
            [ACK] => Ok(Self::Ack),
            [SHORT, control, address, sum, STOP] => {
                if control.wrapping_add(*address) != *sum {
                    return Err(protocol_error(
                        "a short frame whose checksum does not add up",
                    ));
                }
                Ok(Self::Short {
                    control: *control,
                    address: *address,
                })
            }
            [LONG, length, again, LONG, body @ .., sum, STOP] => {
                if length != again || usize::from(*length) != body.len() || body.len() < 3 {
                    return Err(protocol_error("a long frame whose length does not match"));
                }
                if checksum(body) != *sum {
                    return Err(protocol_error(
                        "a long frame whose checksum does not add up",
                    ));
                }
                Ok(Self::Long {
                    control: body[0],
                    address: body[1],
                    ci: body[2],
                    data: body[3..].to_vec(),
                })
            }
            _ => Err(protocol_error("bytes that are no M-Bus frame")),
        }
    }

    /// How many bytes follow the first: none after an acknowledgement,
    /// four after a short frame's start, and after a long frame's start
    /// three more of header before the length is known.
    ///
    /// # Errors
    /// A first byte that opens no frame.
    pub fn after_start(first: u8) -> Result<usize> {
        match first {
            ACK => Ok(0),
            SHORT => Ok(4),
            LONG => Ok(3),
            other => Err(protocol_error(format!("{other:#04x} opens no M-Bus frame"))),
        }
    }

    /// How many bytes follow a long frame's four-byte header: the length,
    /// then the checksum and the stop.
    #[must_use]
    pub const fn after_long_header(length: u8) -> usize {
        length as usize + 2
    }

    /// How long the frame opening `read` is, once its first bytes say:
    /// the rule a serial line reads M-Bus by, since M-Bus delimits nothing
    /// (the serial technology's `Framing::Measured`).
    ///
    /// # Errors
    /// A first byte that opens no M-Bus frame.
    pub fn measure(read: &[u8]) -> Result<Option<usize>> {
        let Some(first) = read.first() else {
            return Ok(None);
        };
        let after = Self::after_start(*first)?;
        if *first != LONG {
            return Ok(Some(1 + after));
        }
        Ok(read
            .get(1)
            .map(|length| 4 + Self::after_long_header(*length)))
    }
}

/// The arithmetic checksum: the sum modulo 256.
#[must_use]
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_frame_encodes_and_decodes_back() {
        let frames = [
            Frame::Ack,
            Frame::Short {
                control: REQ_UD2,
                address: 1,
            },
            Frame::Long {
                control: RSP_UD,
                address: 1,
                ci: 0x72,
                data: vec![1, 2, 3],
            },
            Frame::Long {
                control: SND_UD,
                address: BROADCAST,
                ci: 0x51,
                data: vec![],
            },
        ];
        for frame in frames {
            let bytes = frame.encode().expect("encode");
            assert_eq!(Frame::decode(&bytes).expect("decode"), frame, "{frame:?}");
        }
        let short = Frame::Short {
            control: SND_NKE,
            address: 5,
        };
        assert_eq!(
            short.encode().expect("encode"),
            [0x10, 0x40, 0x05, 0x45, 0x16]
        );
        let long = Frame::Long {
            control: 0x08,
            address: 0x01,
            ci: 0x72,
            data: vec![0x0d, 0x7f, 0x01, 0x41],
        };
        assert_eq!(
            long.encode().expect("encode"),
            [
                0x68, 0x07, 0x07, 0x68, 0x08, 0x01, 0x72, 0x0d, 0x7f, 0x01, 0x41, 0x49, 0x16
            ]
        );
    }

    #[test]
    fn a_frame_that_does_not_add_up_is_refused() {
        assert!(
            Frame::decode(&[0x10, 0x40, 0x05, 0x46, 0x16]).is_err(),
            "checksum"
        );
        assert!(
            Frame::decode(&[0x10, 0x40, 0x05, 0x45, 0x17]).is_err(),
            "stop"
        );
        assert!(Frame::decode(&[0x68, 0x04, 0x03, 0x68, 8, 1, 0x72, 0x7b, 0x16]).is_err());
        assert!(Frame::decode(&[0x68, 0x03, 0x03, 0x68, 8, 1, 0x72, 0x7c, 0x16]).is_err());
        assert!(
            Frame::decode(&[0x68, 0x02, 0x02, 0x68, 8, 1, 0x09, 0x16]).is_err(),
            "short"
        );
        assert!(Frame::decode(&[0x99]).is_err(), "no such start");
        assert!(Frame::decode(&[]).is_err(), "nothing");
        let over = Frame::Long {
            control: SND_UD,
            address: 1,
            ci: 0x51,
            data: vec![0; 253],
        };
        assert!(over.encode().is_err(), "over a long frame");
    }

    #[test]
    fn the_start_byte_says_how_much_follows() {
        assert_eq!(Frame::after_start(ACK).expect("ack"), 0);
        assert_eq!(Frame::after_start(SHORT).expect("short"), 4);
        assert_eq!(Frame::after_start(LONG).expect("long"), 3);
        assert!(Frame::after_start(0x00).is_err());
        assert_eq!(Frame::after_long_header(7), 9);
        assert_eq!(checksum(&[0xff, 0x02]), 0x01, "modulo 256");
    }
}
