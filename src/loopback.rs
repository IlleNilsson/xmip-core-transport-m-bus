//! Meters on the serial technology's multi-drop bus, and both ends of one
//! M-Bus exchange on it (ADR-0051).
//!
//! [`OnTheBus`] is a meter as a device on [`serial::bus::Bus`]: it hears
//! every frame the master puts on the bus and answers the ones addressed to
//! it — its primary address, or the broadcast — as a meter on a real pair of
//! wires does; every other meter on the bus keeps silent. The loopback pair
//! is a master and one meter on a fresh bus; the far end is the meter holding
//! what the master wrote, read back a telegram at a time.
//!
//! Until 2026-09-24 the meter sat on a line that echoed, alone, so nothing
//! showed that a meter at another address stays silent (open problem 24).

use std::sync::Arc;

use serial::bus::{Bus, Device};
use transport::error::Result;
use transport::loopback::{FarEnd, LOOPBACK_TIMEOUT, Loopback};
use transport::{Arrived, Transport};

use crate::frame::{Frame, REQ_UD2, RSP_UD, SND_NKE, SND_UD};
use crate::meter::{Identity, Meter};
use crate::record::{CI_DATA_SEND, CI_VARIABLE_LONG};
use crate::{ANSWER_ROOM, MBusTransport};

/// The primary address of the loopback meter.
pub const METER: u8 = 1;

/// A meter as a device on the bus.
pub struct OnTheBus(pub Arc<Meter>);

impl OnTheBus {
    /// What the meter answers `frame` with, if it is for the meter.
    ///
    /// # Errors
    /// User data the meter cannot take, or an answer no frame carries.
    pub fn answer(&self, frame: &Frame) -> Result<Option<Frame>> {
        let meter = &self.0;
        Ok(match frame {
            Frame::Short { control, address }
                if meter.answers_to(*address) && control & 0x0f == SND_NKE & 0x0f =>
            {
                meter.reset();
                Some(Frame::Ack)
            }
            Frame::Short { control, address }
                if meter.answers_to(*address) && control & 0x0f == REQ_UD2 & 0x0f =>
            {
                let access = meter.next_access();
                let mut data = meter.identity().long_header(access).to_vec();
                data.extend(meter.give(ANSWER_ROOM)?);
                Some(Frame::Long {
                    control: RSP_UD,
                    address: meter.primary(),
                    ci: CI_VARIABLE_LONG,
                    data,
                })
            }
            Frame::Long {
                control,
                address,
                ci: CI_DATA_SEND,
                data,
            } if meter.answers_to(*address) && control & 0x0f == SND_UD & 0x0f => {
                meter.take(data)?;
                Some(Frame::Ack)
            }
            _ => None,
        })
    }
}

impl Device for OnTheBus {
    fn hear(&self, frame: &[u8]) -> Result<Option<Vec<u8>>> {
        self.answer(&Frame::decode(frame)?)?
            .map(|answer| answer.encode())
            .transpose()
    }
}

impl MBusTransport {
    /// Both ends on one bus: a master and the meter at [`METER`] that
    /// answers it, on a fresh [`Bus`], the loopback timeout on the master.
    #[must_use]
    pub fn loopback() -> Self {
        let meter = Meter::new(Identity {
            ident: 12_345_678,
            manufacturer: *b"XMP",
            version: 1,
            medium: 7,
        })
        .at_primary(METER);
        let bus = Bus::new("loopback");
        bus.attach(Arc::new(OnTheBus(Arc::new(meter))));
        Self::new(Arc::new(bus), METER).timing_out_after(LOOPBACK_TIMEOUT)
    }
}

/// The meter on the bus, holding what the master wrote until it is read
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

    /// A fresh master on the same bus writes to the meter at `address`.
    fn send_to(&self, address: &str, payload: &[u8]) -> Result<()> {
        Self::new(Arc::clone(&self.line), self.address)
            .timing_out_after(self.timeout)
            .send(address, payload)
    }

    fn unblock(&self, _address: &str) {
        // The bus is in-process; nothing listens on a socket.
    }

    /// In order on one thread: a bus has one master, so the write goes
    /// first and the read-back finds what it left.
    fn round(&self, payload: &[u8]) -> Result<Arrived> {
        self.round_in_order(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use transport::line::Line;
    use transport::payload::edge_payloads;

    fn meter(ident: u32, primary: u8) -> Arc<Meter> {
        Arc::new(
            Meter::new(Identity {
                ident,
                manufacturer: *b"ABC",
                version: 0,
                medium: 3,
            })
            .at_primary(primary),
        )
    }

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
    fn on_one_bus_the_addressed_meter_answers_and_the_other_keeps_silent() {
        let (three, five) = (meter(3, 3), meter(5, 5));
        let bus = Arc::new(Bus::new("rs485"));
        bus.attach(Arc::new(OnTheBus(Arc::clone(&three))));
        bus.attach(Arc::new(OnTheBus(Arc::clone(&five))));
        let master = MBusTransport::new(Arc::clone(&bus) as Arc<dyn Line>, 1)
            .timing_out_after(Duration::from_millis(10));

        assert!(
            master.initialise(1).expect_err("silence").retryable,
            "nobody at 1"
        );
        master.initialise(3).expect("addressed");
        master.initialise(5).expect("addressed");
        MBusTransport::new(Arc::clone(&bus) as Arc<dyn Line>, 3)
            .write_stream(3, b"for three")
            .expect("three takes it");
        assert_eq!(three.held(), b"for three");
        assert!(five.held().is_empty(), "five heard it and kept silent");

        let at_five = MBusTransport::new(Arc::clone(&bus) as Arc<dyn Line>, 5);
        assert!(
            at_five
                .read_stream()
                .expect("five answers")
                .bytes
                .is_empty()
        );
        let at_three = MBusTransport::new(bus as Arc<dyn Line>, 3);
        let read = at_three.read_stream().expect("three answers");
        assert_eq!(read.bytes, b"for three");
        assert_eq!(read.origin_uri, "mbus://rs485/3");
    }

    #[test]
    fn a_broadcast_reaches_every_meter_and_a_break_is_an_error() {
        let (three, five) = (meter(3, 3), meter(5, 5));
        let bus = Arc::new(Bus::new("rs485"));
        bus.attach(Arc::new(OnTheBus(Arc::clone(&three))));
        bus.attach(Arc::new(OnTheBus(Arc::clone(&five))));
        let master = MBusTransport::new(Arc::clone(&bus) as Arc<dyn Line>, 3)
            .timing_out_after(Duration::from_millis(10));
        // Both acknowledge a broadcast; two acknowledgements are the same
        // byte, so they overlay into one the master accepts.
        master
            .write_stream(0xff, b"to all")
            .expect("everyone takes it");
        assert_eq!(three.held(), b"to all");
        assert_eq!(five.held(), b"to all");
        bus.break_the_line();
        assert!(master.initialise(3).is_err(), "a break condition");
    }
}
