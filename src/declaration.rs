//! This technology's declaration: the transport guide's "The declaration",
//! `module/core/capability/transport/doc/adding-a-transport.md`. The rate a
//! Location takes unless told otherwise is declared here, and the code takes
//! it from here; the wait is the serial port's, taken by the serial
//! technology's own `declaration` feature.

use xcore::settings::{Applies, Fixed, Kind, Presence, Setting, Settings};

/// The rate a meter answers at unless its Location says another: 2400
/// baud, the one EN 13757-2 meters are delivered speaking.
pub(crate) const BAUD: u32 = 2400;

/// Every setting a Location takes beyond its address, the serial port the
/// line is on, `/dev/ttyUSB0` or `COM3`: the meter on it and the line's
/// rate.
pub const SETTINGS: &Settings = &Settings {
    technology: env!("CARGO_PKG_NAME"),
    settings: &[
        Setting {
            name: "meter",
            kind: Kind::Integer {
                minimum: 0,
                maximum: 250,
            },
            presence: Presence::Required,
            meaning: "The meter's primary address, written to unless a target names one.",
            applies: Applies::Both,
        },
        Setting {
            name: "baud",
            kind: Kind::Integer {
                minimum: 300,
                maximum: 38_400,
            },
            presence: Presence::Default(Fixed::Integer(BAUD as i64)),
            meaning: "The rate the line runs at, in baud.",
            applies: Applies::Both,
        },
        // The serial port's own wait, which the master hands each read
        // of the line.
        transport::configured::timeout(
            Applies::Both,
            Some(serial::declaration::TIMEOUT),
            "How long a meter that does not answer is waited for.",
        ),
    ],
};

/// This technology's schemes, as [`transport::Configured::SCHEMES`] defines
/// them.
pub const SCHEMES: &[&str] = &["mbus"];
