#![forbid(unsafe_code)]

//! Streams written to a meter and read back from it, the way wired M-Bus
//! moves them.
//!
//! M-Bus is the metering bus of EN 13757: heat, water, gas and electricity
//! meters on a two-wire line, a master asking and one slave at a time
//! answering. What is here is the wired link layer — the acknowledgement,
//! the short frame and the long frame, a checksum that adds up — and the
//! application layer above it: a Stream as variable data records, over as
//! many telegrams as it takes, closed by `SND_UD` and read back by
//! `REQ_UD2` and `RSP_UD`. A Send Location writes a Stream to a meter; a
//! Receive Location reads the one a meter holds. There is no ceiling: a
//! Stream is as long as its telegrams.
//!
//! The carrier is a [`Line`]: a serial port on a deployment, framed by
//! [`Frame::measure`], or the serial technology's multi-drop bus in
//! process, with meters on it at their own addresses. A master and a meter
//! round-trip on that bus with no level converter, which is what
//! [`MBusTransport::loopback`] stands up (ADR-0051); more meters on one bus
//! show what the wire is for — the one addressed answers, the rest keep
//! silent (open problem 24).
//! Wireless M-Bus frames the same records and the same meter over the air
//! and rides on this crate for them.
//!
//! The origin URI names the line and the meter's address:
//! `mbus://<line>/<address>`.

pub mod frame;
pub mod loopback;
pub mod meter;
pub mod record;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use transport::error::{Result, protocol_error};
use transport::line::Line;
use transport::{Arrived, Directions, Transport};

pub use frame::Frame;
pub use meter::{Identity, Meter};

use crate::frame::{FCB, MAX_USER_DATA, REQ_UD2, RSP_UD, SND_NKE, SND_UD};
use crate::record::{CI_DATA_SEND, CI_VARIABLE_LONG};

/// The room a wired answer leaves for records: a long frame's user data
/// less the long header.
pub const ANSWER_ROOM: usize = MAX_USER_DATA - 12;

/// The master's side of a line.
#[derive(Clone)]
pub struct MBusTransport {
    line: Arc<dyn Line>,
    address: u8,
    timeout: Duration,
    fcb: Arc<AtomicBool>,
}

impl MBusTransport {
    /// A master on `line`, talking to the meter at primary `address`.
    #[must_use]
    pub fn new(line: Arc<dyn Line>, address: u8) -> Self {
        Self {
            line,
            address,
            timeout: Duration::from_secs(1),
            fcb: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Give up on a meter that does not answer within `timeout`.
    #[must_use]
    pub const fn timing_out_after(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// `mbus://<line>/<address>`.
    #[must_use]
    pub fn origin(&self, address: u8) -> String {
        format!("mbus://{}/{address}", self.line.name())
    }

    /// Send `frame` and take the meter's answer.
    ///
    /// # Errors
    /// No answer in time, or an answer that is no frame.
    pub fn exchange(&self, frame: &Frame) -> Result<Frame> {
        self.line.transmit(&frame.encode()?)?;
        let answer = self
            .line
            .receive(self.timeout)?
            .ok_or_else(|| transport::TransportError::retryable("the meter did not answer"))?;
        let answer = Frame::decode(&answer)?;
        self.fcb.fetch_xor(true, Ordering::Relaxed);
        Ok(answer)
    }

    /// The control field with this call's frame count bit.
    fn control(&self, base: u8) -> u8 {
        if self.fcb.load(Ordering::Relaxed) {
            base | FCB
        } else {
            base
        }
    }

    /// `SND_NKE`: initialise the meter at `address`.
    ///
    /// # Errors
    /// No acknowledgement.
    pub fn initialise(&self, address: u8) -> Result<()> {
        let frame = Frame::Short {
            control: SND_NKE,
            address,
        };
        self.fcb.store(false, Ordering::Relaxed);
        acknowledged(&self.exchange(&frame)?)?;
        self.fcb.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Write `bytes` to the meter at `address`, a telegram of records at a
    /// time, each acknowledged.
    ///
    /// # Errors
    /// A telegram not acknowledged.
    pub fn write_stream(&self, address: u8, bytes: &[u8]) -> Result<()> {
        for data in record::pack(bytes, MAX_USER_DATA)? {
            let frame = Frame::Long {
                control: self.control(SND_UD),
                address,
                ci: CI_DATA_SEND,
                data,
            };
            acknowledged(&self.exchange(&frame)?)?;
        }
        Ok(())
    }

    /// Read the Stream the meter holds, a telegram of records at a time.
    ///
    /// # Errors
    /// An answer that is not the meter's data, or records that are no
    /// Stream.
    pub fn read_stream(&self) -> Result<Arrived> {
        let mut bytes = Vec::new();
        loop {
            let request = Frame::Short {
                control: self.control(REQ_UD2),
                address: self.address,
            };
            let (chunk, more) = match self.exchange(&request)? {
                Frame::Long {
                    control,
                    ci: CI_VARIABLE_LONG,
                    data,
                    ..
                } if control & 0x0f == RSP_UD => record::unpack(data.get(12..).unwrap_or(&[]))?,
                other => {
                    return Err(protocol_error(format!("{other:?} is not the meter's data")));
                }
            };
            bytes.extend_from_slice(&chunk);
            if !more {
                return Ok(Arrived::new(self.origin(self.address), bytes));
            }
        }
    }
}

fn acknowledged(answer: &Frame) -> Result<()> {
    if *answer == Frame::Ack {
        Ok(())
    } else {
        Err(protocol_error(format!(
            "{answer:?} where an acknowledgement was due"
        )))
    }
}

impl Transport for MBusTransport {
    fn name(&self) -> &'static str {
        "m-bus"
    }

    fn directions(&self) -> Directions {
        Directions::BOTH
    }

    /// Read the Stream the meter holds.
    fn receive(&self) -> Result<Vec<Arrived>> {
        Ok(vec![self.read_stream()?])
    }

    /// `target` may name an address, `mbus://line/7`, overriding the
    /// transport's.
    fn send(&self, target: &str, bytes: &[u8]) -> Result<()> {
        let address = match transport::socket::target("mbus", target) {
            Some((_, address)) if !address.is_empty() => address
                .parse()
                .map_err(|_| protocol_error(format!("{address} is not a primary address")))?,
            _ => self.address,
        };
        self.write_stream(address, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial::{Framing, SerialTransport};
    use transport::loopback::LOOPBACK_TIMEOUT;

    /// A serial loopback wire read as M-Bus reads a port: by the length
    /// the frame opens with.
    fn port() -> SerialTransport {
        SerialTransport::loopback().framed(Framing::Measured(Frame::measure))
    }

    #[test]
    fn a_serial_port_is_a_line_that_reads_frames_by_their_own_length() {
        let port = port();
        assert_eq!(Line::name(&port), "loopback");
        assert!(
            Line::receive(&port, LOOPBACK_TIMEOUT)
                .expect("quiet")
                .is_none()
        );
        let long = Frame::Long {
            control: RSP_UD,
            address: 1,
            ci: 0x72,
            data: vec![0x16, 0xe5, 0x10],
        };
        let short = Frame::Short {
            control: SND_NKE,
            address: 1,
        };
        for frame in [&long, &Frame::Ack, &short] {
            Line::transmit(&port, &frame.encode().expect("encode")).expect("transmit");
        }
        for frame in [long, Frame::Ack, short] {
            let bytes = Line::receive(&port, LOOPBACK_TIMEOUT)
                .expect("receive")
                .expect("a frame");
            assert_eq!(Frame::decode(&bytes).expect("decode"), frame);
        }
        Line::transmit(&port, &[0x99]).expect("transmit");
        assert!(
            Line::receive(&port, LOOPBACK_TIMEOUT).is_err(),
            "opens no frame"
        );
    }

    #[test]
    fn a_frame_cut_short_is_an_error_not_silence() {
        let port = port();
        Line::transmit(&port, &[0x10, 0x40]).expect("half a frame");
        assert!(Line::receive(&port, LOOPBACK_TIMEOUT).is_err(), "cut short");
    }

    #[test]
    fn a_master_names_itself_and_takes_an_address_from_the_target() {
        let master =
            MBusTransport::new(Arc::new(port()), 1).timing_out_after(Duration::from_millis(10));
        assert_eq!(master.name(), "m-bus");
        assert!(master.claims().is_none());
        assert!(master.directions().receives() && master.directions().sends());
        assert_eq!(master.origin(1), "mbus://loopback/1");
        assert!(
            master.send("mbus://loopback/x", b"").is_err(),
            "not an address"
        );
        let error = master.send("mbus://loopback/7", b"").expect_err("no meter");
        assert!(
            error.message.contains("acknowledgement"),
            "a bare line echoes the master: {error}"
        );
        assert!(master.read_stream().is_err(), "no meter");
    }
}
