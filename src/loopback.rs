//! A meter on the serial loopback line, and both ends of one M-Bus
//! exchange on it (ADR-0051).
//!
//! [`LoopbackLine`] is the serial technology's in-memory line with a meter
//! listening: what the master transmits goes down the line, the meter
//! reads it off and puts its answer back, and the answer is what the
//! master receives next. The loopback pair is a master on that line and
//! the meter it writes a Stream to; the far end is the meter holding it,
//! read back a telegram at a time. One line, one master, one thread: the
//! round goes in order.

use std::sync::Arc;
use std::time::Duration;

use serial::SerialTransport;
use transport::error::{Result, protocol_error};
use transport::loopback::{FarEnd, LOOPBACK_TIMEOUT, Loopback};
use transport::{Arrived, Transport};

use crate::frame::{Frame, REQ_UD2, RSP_UD, SND_NKE, SND_UD};
use crate::meter::{Identity, Meter};
use crate::record::{CI_DATA_SEND, CI_VARIABLE_LONG};
use crate::{ANSWER_ROOM, Line, MBusTransport};

/// The primary address of the loopback meter.
pub const METER: u8 = 1;

/// A meter listening on the serial loopback line.
pub struct LoopbackLine {
    line: SerialTransport,
    meter: Arc<Meter>,
}

impl LoopbackLine {
    /// `meter` on a fresh serial loopback line.
    #[must_use]
    pub fn new(meter: Meter) -> Self {
        Self {
            line: SerialTransport::loopback(),
            meter: Arc::new(meter),
        }
    }

    #[must_use]
    pub fn meter(&self) -> &Meter {
        &self.meter
    }

    /// What the meter answers `frame` with, if it is for the meter.
    ///
    /// # Errors
    /// User data the meter cannot take, or an answer no frame carries.
    pub fn answer(&self, frame: &Frame) -> Result<Option<Frame>> {
        Ok(match frame {
            Frame::Short { control, address }
                if self.meter.answers_to(*address) && control & 0x0f == SND_NKE & 0x0f =>
            {
                self.meter.reset();
                Some(Frame::Ack)
            }
            Frame::Short { control, address }
                if self.meter.answers_to(*address) && control & 0x0f == REQ_UD2 & 0x0f =>
            {
                let access = self.meter.next_access();
                let mut data = self.meter.identity().long_header(access).to_vec();
                data.extend(self.meter.give(ANSWER_ROOM)?);
                Some(Frame::Long {
                    control: RSP_UD,
                    address: self.meter.primary(),
                    ci: CI_VARIABLE_LONG,
                    data,
                })
            }
            Frame::Long {
                control,
                address,
                ci: CI_DATA_SEND,
                data,
            } if self.meter.answers_to(*address) && control & 0x0f == SND_UD & 0x0f => {
                self.meter.take(data)?;
                Some(Frame::Ack)
            }
            _ => None,
        })
    }
}

impl Line for LoopbackLine {
    fn name(&self) -> String {
        Line::name(&self.line)
    }

    /// Down the serial line, off it again as the meter, and the answer
    /// back on it for the master.
    fn transmit(&self, frame: &[u8]) -> Result<()> {
        self.line.transmit(frame)?;
        let heard = Line::receive(&self.line, Duration::ZERO)?
            .ok_or_else(|| protocol_error("transmitted, but nothing came down the line"))?;
        if let Some(answer) = self.answer(&Frame::decode(&heard)?)? {
            self.line.transmit(&answer.encode()?)?;
        }
        Ok(())
    }

    fn receive(&self, timeout: Duration) -> Result<Option<Vec<u8>>> {
        Line::receive(&self.line, timeout)
    }
}

impl MBusTransport {
    /// Both ends on one line: a master and the meter at [`METER`] that
    /// answers it, on a fresh [`LoopbackLine`], the loopback timeout on the
    /// master.
    #[must_use]
    pub fn loopback() -> Self {
        let meter = Meter::new(Identity {
            ident: 12_345_678,
            manufacturer: *b"XMP",
            version: 1,
            medium: 7,
        })
        .at_primary(METER);
        Self::new(Arc::new(LoopbackLine::new(meter)), METER).timing_out_after(LOOPBACK_TIMEOUT)
    }
}

/// The meter on the line, holding what the master wrote until it is read
/// back.
struct Holding {
    master: MBusTransport,
    address: String,
}

impl FarEnd for Holding {
    fn address(&self) -> &str {
        &self.address
    }

    fn take_one(self: Box<Self>) -> Result<Arrived> {
        self.master.read_stream()
    }
}

impl Loopback for MBusTransport {
    fn far_end(&self) -> Result<Box<dyn FarEnd>> {
        Ok(Box::new(Holding {
            master: self.clone(),
            address: self.origin(self.address),
        }))
    }

    /// A fresh master on the same line writes to the meter at `address`.
    fn send_to(&self, address: &str, payload: &[u8]) -> Result<()> {
        Self::new(Arc::clone(&self.line), self.address)
            .timing_out_after(self.timeout)
            .send(address, payload)
    }

    fn unblock(&self, _address: &str) {
        // The line is in-process; nothing listens on a socket.
    }

    /// In order on one thread: a line has one master, so the write goes
    /// first and the read-back finds what it left.
    fn round(&self, payload: &[u8]) -> Result<Arrived> {
        self.round_in_order(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use transport::payload::edge_payloads;

    #[test]
    fn the_loopback_returns_the_edges_whole() {
        let loopback = MBusTransport::loopback();
        for (name, bytes) in edge_payloads() {
            let arrived = loopback
                .round(&bytes)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(arrived.bytes, bytes, "{name}");
            assert_eq!(arrived.origin_uri, "mbus://loopback/1", "{name}");
        }
        assert!(
            loopback.ceiling().is_none(),
            "as many telegrams as it takes"
        );
        assert!(loopback.refuses(b"\x16\xe5").is_none());
    }

    #[test]
    fn a_long_stream_crosses_in_many_telegrams_both_ways() {
        let loopback = MBusTransport::loopback();
        let long: Vec<u8> = (0..3000u32)
            .map(|n| u8::try_from(n % 253).unwrap_or(0))
            .collect();
        let arrived = loopback.round(&long).expect("sixteen telegrams each way");
        assert_eq!(arrived.bytes, long);
        loopback.initialise(METER).expect("SND_NKE");
        assert_eq!(loopback.read_stream().expect("still held").bytes, long);
    }

    #[test]
    fn a_meter_that_is_not_addressed_does_not_answer() {
        let identity = Identity {
            ident: 1,
            manufacturer: *b"ABC",
            version: 0,
            medium: 3,
        };
        let line = Arc::new(LoopbackLine::new(Meter::new(identity).at_primary(3)));
        let master = MBusTransport::new(Arc::clone(&line) as Arc<dyn Line>, 1)
            .timing_out_after(Duration::from_millis(10));
        assert!(master.initialise(1).expect_err("silence").retryable);
        master.initialise(3).expect("addressed");
        master
            .write_stream(0xff, b"broadcast")
            .expect("everyone takes it");
        assert_eq!(line.meter().held(), b"broadcast");
        assert!(master.read_stream().is_err(), "address 1 is nobody");
        let found = MBusTransport::new(line, 3);
        assert_eq!(found.read_stream().expect("read").bytes, b"broadcast");
        assert_eq!(
            found.read_stream().expect("read").origin_uri,
            "mbus://loopback/3"
        );
    }
}
