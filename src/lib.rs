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
//! **A receive is a read, `REQ_UD2`, which consumes nothing at the
//! meter** — it keeps holding the Stream — so the verdict has nothing to
//! tell it, whichever it is: a cycle that did not complete loses nothing, and
//! the next read finds the Stream again.
//!
//! The origin URI names the line and the meter's address:
//! `mbus://<line>/<address>`.
//!
//! **Built with its `declaration` feature alone** the crate is
//! [`declaration`].

#[cfg(feature = "declaration")]
pub mod declaration;
#[cfg(feature = "default")]
pub mod frame;
#[cfg(feature = "default")]
mod frame_count;
#[cfg(feature = "default")]
pub mod loopback;
#[cfg(feature = "default")]
pub mod meter;
#[cfg(feature = "default")]
pub mod record;

#[cfg(feature = "default")]
use std::sync::Arc;
#[cfg(feature = "default")]
use std::time::Duration;

#[cfg(feature = "default")]
use context::property::M_BUS_PRIMARY_ADDRESS;
#[cfg(feature = "default")]
use declaration::BAUD;
#[cfg(feature = "default")]
use net::Target;
#[cfg(feature = "default")]
use serial::{Framing, SerialTransport};
#[cfg(feature = "default")]
use transport::error::{Result, protocol_error};
#[cfg(feature = "default")]
use transport::line::Line;
#[cfg(feature = "default")]
use transport::{Acknowledgement, Arrived, Configured, Directions, Sharing, Taken, Transport};
#[cfg(feature = "default")]
use xcore::lock::Locked;
#[cfg(feature = "default")]
use xcore::settings::{Read, Settings};

#[cfg(feature = "default")]
pub use frame::Frame;
#[cfg(feature = "default")]
pub use meter::{Identity, Meter};

#[cfg(feature = "default")]
use crate::frame::{FCB, MAX_USER_DATA, REQ_UD2, RSP_UD, SND_NKE, SND_UD};
#[cfg(feature = "default")]
use crate::frame_count::FrameCounts;
#[cfg(feature = "default")]
use crate::record::{CI_DATA_SEND, CI_VARIABLE_LONG};

/// The room a wired answer leaves for records: a long frame's user data
/// less the long header.
#[cfg(feature = "default")]
pub const ANSWER_ROOM: usize = MAX_USER_DATA - 12;

/// The master's side of a line.
#[cfg(feature = "default")]
#[derive(Clone)]
pub struct MBusTransport {
    line: Arc<dyn Line>,
    address: u8,
    timeout: Duration,
    /// The frame count bit of each meter the line reaches: the port's, on a
    /// port the node's Locations share (`frame_count.rs`).
    counts: Arc<FrameCounts>,
}

#[cfg(feature = "default")]
impl MBusTransport {
    /// A master on `line`, talking to the meter at primary `address`,
    /// waiting on it as long as a serial port waits on a read
    /// ([`serial::declaration::TIMEOUT`]) until told otherwise.
    #[must_use]
    pub fn new(line: Arc<dyn Line>, address: u8) -> Self {
        Self {
            line,
            address,
            timeout: serial::declaration::TIMEOUT,
            counts: Arc::default(),
        }
    }

    /// `mbus://<line>/<address>`.
    #[must_use]
    pub fn origin(&self, address: u8) -> String {
        format!("mbus://{}/{address}", self.line.name())
    }

    /// Send `frame` as it is and take the meter's answer: its frame count
    /// bit is the caller's.
    ///
    /// # Errors
    /// No answer in time, or an answer that is no frame.
    pub fn exchange(&self, frame: &Frame) -> Result<Frame> {
        // One exchange on the line, so on a port other Locations share the
        // meter's answer reaches this master alone.
        let answer = self
            .line
            .exchange(&frame.encode()?, self.timeout)?
            .ok_or_else(|| transport::TransportError::retryable("the meter did not answer"))?;
        Frame::decode(&answer)
    }

    /// The frame `frame` builds on `base`, the control with the frame count
    /// bit of the meter at `address`, sent and answered: the bit toggled
    /// once the meter answered, so the next call to the meter, from any
    /// Location reaching it, carries the other. The meter's bit is held
    /// across the call (`frame_count.rs`).
    fn counted(&self, address: u8, base: u8, frame: impl FnOnce(u8) -> Frame) -> Result<Frame> {
        let meter = self.counts.of(address);
        let mut bit = meter.locked();
        let control = if *bit { base | FCB } else { base };
        let answer = self.exchange(&frame(control))?;
        *bit = !*bit;
        Ok(answer)
    }

    /// `SND_NKE`: initialize the meter at `address`, whose next call then
    /// carries the frame count bit set.
    ///
    /// # Errors
    /// No acknowledgement.
    pub fn initialize(&self, address: u8) -> Result<()> {
        let frame = Frame::Short {
            control: SND_NKE,
            address,
        };
        let meter = self.counts.of(address);
        let mut bit = meter.locked();
        *bit = false;
        let answer = self.exchange(&frame)?;
        *bit = true;
        drop(bit);
        acknowledged(&answer)
    }

    /// Write `bytes` to the meter at `address`, a telegram of records at a
    /// time, each acknowledged.
    ///
    /// # Errors
    /// A telegram not acknowledged.
    pub fn write_stream(&self, address: u8, bytes: &[u8]) -> Result<()> {
        for data in record::pack(bytes, MAX_USER_DATA)? {
            let answer = self.counted(address, SND_UD, |control| Frame::Long {
                control,
                address,
                ci: CI_DATA_SEND,
                data,
            })?;
            acknowledged(&answer)?;
        }
        Ok(())
    }

    /// Read the Stream the meter holds, whole, a telegram of records at a
    /// time. The meter keeps holding it.
    ///
    /// # Errors
    /// An answer that is not the meter's data, or records that are no
    /// Stream.
    pub fn read_stream(&self) -> Result<Taken> {
        let mut bytes = Vec::new();
        loop {
            let request = |control| Frame::Short {
                control,
                address: self.address,
            };
            let (chunk, more) = match self.counted(self.address, REQ_UD2, request)? {
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
                return Ok(Taken::new(self.origin(self.address), bytes)
                    .observing(M_BUS_PRIMARY_ADDRESS, self.address.to_string()));
            }
        }
    }
}

#[cfg(feature = "default")]
impl Configured for MBusTransport {
    const SETTINGS: &'static Settings = declaration::SETTINGS;
    const SCHEMES: &'static [&'static str] = declaration::SCHEMES;

    fn timeout_kept(&mut self) -> Option<&mut dyn transport::configured::TimeoutKept> {
        Some(&mut self.timeout)
    }

    /// The line on what the Locations of the node share: a serial port
    /// takes the one port its device has on the node, so every meter on one
    /// bus is read through it, and the frame count bit of each meter on it
    /// is kept with the port, under its path, so every Location reaching a
    /// meter alternates one bit; a line nothing is shared on stays as it
    /// is, its bits the Location's own.
    fn shared_by(mut self, sharing: &Arc<dyn Sharing>) -> Self {
        if let Some(line) = self.line.shared_on(sharing) {
            self.counts = sharing.one(&line.name(), FrameCounts::default);
            self.line = line;
        }
        self
    }

    fn configured(address: &str, settings: &Read) -> Result<Self> {
        // The declaration holds both integers within their ranges.
        let baud = u32::try_from(settings.integer("baud")).unwrap_or(BAUD);
        let meter = u8::try_from(settings.integer("meter")).unwrap_or(0);
        // Each read of the port waits what the master hands it: the
        // Location's timeout, which `open` hands the master after this.
        let port = SerialTransport::new(address, baud).framed(Framing::Measured(Frame::measure));
        Ok(Self::new(Arc::new(port), meter))
    }
}

#[cfg(feature = "default")]
fn acknowledged(answer: &Frame) -> Result<()> {
    if *answer == Frame::Ack {
        Ok(())
    } else {
        Err(protocol_error(format!(
            "{answer:?} where an acknowledgement was due"
        )))
    }
}

#[cfg(feature = "default")]
impl Transport for MBusTransport {
    fn name(&self) -> &'static str {
        "m-bus"
    }

    fn directions(&self) -> Directions {
        Directions::BOTH
    }

    fn arrivals(&self) -> transport::Arrivals {
        transport::Arrivals::Ordered("a poll reads again what is not yet told")
    }

    fn waiting(&self) -> transport::Waiting {
        transport::Waiting::Polled("a read of the meter returns at once")
    }

    /// Read the Stream the meter holds, whole. The verdict has nothing to
    /// tell the meter, whichever it is: `REQ_UD2` consumes nothing — the
    /// meter keeps holding the Stream — so a cycle that did not complete
    /// loses nothing, and the next read finds it again.
    fn receive_up_to(&self, _most: usize) -> Result<Vec<Arrived>> {
        let read = self.read_stream()?;
        Ok(vec![
            Arrived::whole(read.origin_uri, read.bytes, Acknowledgement::unconsumed())
                .scheduled()
                .observing_all(read.observed),
        ])
    }

    fn sending(&self) -> transport::Sending {
        transport::Sending::Gathered("one M-Bus telegram carries the whole body")
    }

    /// `target` may name an address, `mbus://line/7`, overriding the
    /// transport's.
    fn send_from(&self, target: &str, outgoing: transport::Outgoing<'_>) -> Result<()> {
        let bytes: &[u8] = &outgoing.gather()?;
        let address = match Target::under(Self::SCHEMES, target)
            .map(|named| (named.authority(), named.path()))
        {
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
    use transport::loopback::LOOPBACK_TIMEOUT;
    use xcore::lock::Locked;
    use xcore::settings::Applies;

    /// A serial loopback wire read as M-Bus reads a port: by the length
    /// the frame opens with.
    fn port() -> SerialTransport {
        SerialTransport::loopback().framed(Framing::Measured(Frame::measure))
    }

    #[test]
    fn m_bus_declares_its_settings_and_reads_through_them() {
        use xcore::settings::Given;
        assert_eq!(MBusTransport::SETTINGS.problems(), [] as [String; 0]);
        let given = [
            ("meter".to_string(), Given::Integer(7)),
            ("timeout".to_string(), Given::Text("2s".to_string())),
        ];
        let built = MBusTransport::open("/dev/ttyUSB0", Applies::Receive, &given).expect("built");
        assert_eq!(built.line.name(), "/dev/ttyUSB0");
        assert_eq!(built.address, 7);
        assert_eq!(built.timeout, Duration::from_secs(2));
        assert_eq!(built.origin(7), "mbus:///dev/ttyUSB0/7");
        // Left out, the meter is waited on as long as the serial port waits
        // on a read: the default the Location declares.
        let declared = MBusTransport::SETTINGS;
        let unsaid = declared.read(Applies::Receive, &given[..1]).expect("read");
        let timeout = unsaid.optional_duration(transport::configured::TIMEOUT);
        assert_eq!(timeout, Some(serial::declaration::TIMEOUT));
        let unsaid = MBusTransport::open("COM3", Applies::Receive, &given[..1]).expect("built");
        assert_eq!(unsaid.timeout, serial::declaration::TIMEOUT);
        let Err(refused) = MBusTransport::open("COM3", Applies::Send, &given[1..]) else {
            panic!("meter is required");
        };
        assert!(refused.message.contains("\"meter\""), "{refused}");
    }

    /// A line that never answers, keeping the wait each read was handed.
    #[derive(Default)]
    struct Silent(std::sync::Mutex<Vec<Duration>>);

    impl Line for Silent {
        fn name(&self) -> String {
            "silent".to_string()
        }

        fn transmit(&self, _frame: &[u8]) -> Result<()> {
            Ok(())
        }

        fn receive(&self, timeout: Duration) -> Result<Option<Vec<u8>>> {
            self.0.locked().push(timeout);
            Ok(None)
        }
    }

    /// A Location's timeout is what the master hands every read of its
    /// line, which the serial port sets on itself, kept open between the
    /// master's transmit and its receive.
    #[test]
    fn a_configured_timeout_is_what_every_read_of_the_line_waits() {
        use xcore::settings::Given;
        let given = [
            ("meter".to_string(), Given::Integer(7)),
            ("timeout".to_string(), Given::Text("250ms".to_string())),
        ];
        let built = MBusTransport::open("COM3", Applies::Receive, &given).expect("built");
        // The port the Location names, handed in as a line that never
        // answers.
        let silent = Arc::new(Silent::default());
        let master = MBusTransport {
            line: Arc::clone(&silent) as Arc<dyn Line>,
            ..built
        };
        let error = master.read_stream().expect_err("no answer");
        assert!(error.message.contains("did not answer"), "{error}");
        assert_eq!(*silent.0.locked(), [Duration::from_millis(250)]);
    }

    /// Two M-Bus Locations naming one port take its one port from what
    /// their node shares, so what one master puts on the line the other
    /// reads off it, where a port of its own reads nothing; a line nothing
    /// is shared on stays as it is.
    #[test]
    fn locations_on_one_port_take_its_one_port_from_their_node() {
        let node: Arc<dyn Sharing> = Arc::new(transport::Shared::default());
        let first = MBusTransport::new(Arc::new(port()), 1).shared_by(&node);
        let second = MBusTransport::new(Arc::new(port()), 2).shared_by(&node);
        let alone = MBusTransport::new(Arc::new(port()), 3);
        let ack = Frame::Ack.encode().expect("encode");
        first.line.transmit(&ack).expect("transmit");
        let read = alone.line.receive(LOOPBACK_TIMEOUT).expect("quiet");
        assert_eq!(read, None, "a port of its own");
        let read = second.line.receive(LOOPBACK_TIMEOUT).expect("read");
        assert_eq!(read, Some(ack), "one port for both");
        let silent = MBusTransport::new(Arc::new(Silent::default()), 4).shared_by(&node);
        assert_eq!(silent.line.name(), "silent");
    }

    /// A port the Locations of a node share, with meters on it: each frame
    /// put on it is answered at once by the meter it is for, and the
    /// frame's address and control are kept.
    #[derive(Clone)]
    struct Metered(Arc<Meters>);

    /// The meters on a [`Metered`] port, and each frame's address and
    /// control, in order.
    struct Meters {
        on: Vec<loopback::OnTheBus>,
        heard: std::sync::Mutex<Vec<(u8, u8)>>,
    }

    impl Metered {
        /// Meters at `addresses`, holding nothing.
        fn with(addresses: &[u8]) -> Self {
            let meter = |address: u8| {
                let identity = Identity {
                    ident: u32::from(address),
                    manufacturer: *b"XMP",
                    version: 1,
                    medium: 7,
                };
                loopback::OnTheBus(Arc::new(Meter::new(identity).at_primary(address)))
            };
            Self(Arc::new(Meters {
                on: addresses.iter().copied().map(meter).collect(),
                heard: std::sync::Mutex::default(),
            }))
        }

        /// Each call's address, and whether its frame count bit was set.
        fn bits(&self) -> Vec<(u8, bool)> {
            let heard = self.0.heard.locked();
            heard
                .iter()
                .map(|&(at, control)| (at, control & FCB != 0))
                .collect()
        }
    }

    impl Line for Metered {
        fn name(&self) -> String {
            "COM7".to_string()
        }

        fn transmit(&self, _frame: &[u8]) -> Result<()> {
            Err(protocol_error("a master exchanges"))
        }

        fn receive(&self, _timeout: Duration) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn exchange(&self, frame: &[u8], _timeout: Duration) -> Result<Option<Vec<u8>>> {
            let frame = Frame::decode(frame)?;
            if let Frame::Short { control, address }
            | Frame::Long {
                control, address, ..
            } = &frame
            {
                self.0.heard.locked().push((*address, *control));
            }
            for meter in &self.0.on {
                if let Some(answer) = meter.answer(&frame)? {
                    return answer.encode().map(Some);
                }
            }
            Ok(None)
        }

        fn shared_on(&self, _sharing: &Arc<dyn Sharing>) -> Option<Arc<dyn Line>> {
            Some(Arc::new(self.clone()))
        }
    }

    /// Two Locations reaching one meter on a port their node shares
    /// alternate one frame count bit, the meter's (EN 13757-2), kept with
    /// the port by its path; a Location on another meter there alternates
    /// that meter's; and a reset of the meter (`SND_NKE`) by one sets the
    /// bit of the next call to it, the other's. Until 2026-11-15 each
    /// Location kept a bit of its own, and the second call to the meter
    /// carried the first's bit again.
    #[test]
    fn two_locations_on_one_meter_alternate_one_frame_count_bit() {
        let node: Arc<dyn Sharing> = Arc::new(transport::Shared::default());
        let port = Metered::with(&[1, 2]);
        let on = |meter| MBusTransport::new(Arc::new(port.clone()), meter).shared_by(&node);
        let (first, second, other) = (on(1), on(1), on(2));
        for master in [&first, &second, &other, &first, &other, &second] {
            master.read_stream().expect("read");
        }
        second.initialize(1).expect("reset");
        first.read_stream().expect("read");
        let bits = [
            (1, true),
            (1, false),
            (2, true),
            (1, true),
            (2, false),
            (1, false),
            (1, false),
            (1, true),
        ];
        assert_eq!(port.bits(), bits);
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
