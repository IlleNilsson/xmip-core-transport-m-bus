//! One meter's worth of EN 13757-3: who it is, and the Stream it holds.
//!
//! A meter is identified by an eight-digit number, a three-letter
//! manufacturer, a version and a medium — the long header of every wired
//! answer, and the address of every wireless frame. What it holds is a
//! Stream: a master writes one in records over as many telegrams as it
//! takes, and reads it back the same way. The meter is the same whether
//! the telegrams came down a wire or over the air; the frame around them
//! is the link layer's business.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, PoisonError};

use transport::error::{Result, protocol_error};

use crate::record;

/// Who a meter is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// Eight decimal digits, as printed on the meter.
    pub ident: u32,
    /// Three capital letters.
    pub manufacturer: [u8; 3],
    pub version: u8,
    /// EN 13757-3's device type: `02` electricity, `03` gas, `07` water.
    pub medium: u8,
}

impl Identity {
    /// The manufacturer as the two bytes of EN 61107: each letter less
    /// sixty-four, in five bits.
    #[must_use]
    pub fn manufacturer_code(&self) -> u16 {
        self.manufacturer.iter().fold(0u16, |code, letter| {
            code * 32 + u16::from(letter.saturating_sub(64) & 0x1f)
        })
    }

    /// The ident as four bytes of packed BCD, least significant first.
    #[must_use]
    pub fn ident_bcd(&self) -> [u8; 4] {
        let mut digits = self.ident % 100_000_000;
        let mut out = [0u8; 4];
        for byte in &mut out {
            let low = u8::try_from(digits % 10).unwrap_or(0);
            let high = u8::try_from((digits / 10) % 10).unwrap_or(0);
            *byte = (high << 4) | low;
            digits /= 100;
        }
        out
    }

    /// The eight bytes a wireless frame names a meter by: the manufacturer,
    /// the ident, the version and the medium.
    #[must_use]
    pub fn link_address(&self) -> [u8; 8] {
        let [m0, m1] = self.manufacturer_code().to_le_bytes();
        let [i0, i1, i2, i3] = self.ident_bcd();
        [m0, m1, i0, i1, i2, i3, self.version, self.medium]
    }

    /// The twelve-byte long header of a wired answer: the ident, the
    /// manufacturer, the version, the medium, the access number, a status
    /// of zero and no signature.
    #[must_use]
    pub fn long_header(&self, access: u8) -> [u8; 12] {
        let [m0, m1] = self.manufacturer_code().to_le_bytes();
        let [i0, i1, i2, i3] = self.ident_bcd();
        [
            i0,
            i1,
            i2,
            i3,
            m0,
            m1,
            self.version,
            self.medium,
            access,
            0,
            0,
            0,
        ]
    }

    /// The four-byte short header of a wireless answer: the access number,
    /// a status of zero and no encryption.
    #[must_use]
    pub const fn short_header(access: u8) -> [u8; 4] {
        [access, 0, 0, 0]
    }

    /// `<manufacturer>-<ident>`, as a Location names the meter.
    #[must_use]
    pub fn label(&self) -> String {
        format!(
            "{}-{:08}",
            String::from_utf8_lossy(&self.manufacturer),
            self.ident
        )
    }
}

/// A meter: an identity, a primary address, and the Stream it holds.
pub struct Meter {
    identity: Identity,
    primary: u8,
    access: AtomicU8,
    held: Mutex<Vec<u8>>,
    arriving: Mutex<Vec<u8>>,
    giving: Mutex<VecDeque<Vec<u8>>>,
}

impl Meter {
    /// A meter at primary address zero, holding nothing.
    #[must_use]
    pub fn new(identity: Identity) -> Self {
        Self {
            identity,
            primary: 0,
            access: AtomicU8::new(0),
            held: Mutex::new(Vec::new()),
            arriving: Mutex::new(Vec::new()),
            giving: Mutex::new(VecDeque::new()),
        }
    }

    #[must_use]
    pub const fn at_primary(mut self, primary: u8) -> Self {
        self.primary = primary;
        self
    }

    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    #[must_use]
    pub const fn primary(&self) -> u8 {
        self.primary
    }

    /// Whether a telegram at `address` is for this meter: its own, or
    /// either broadcast.
    #[must_use]
    pub const fn answers_to(&self, address: u8) -> bool {
        address == self.primary
            || address == crate::frame::BROADCAST_REPLY
            || address == crate::frame::BROADCAST
    }

    /// The Stream the meter holds, as the last complete write left it.
    #[must_use]
    pub fn held(&self) -> Vec<u8> {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Forget a write in progress and a read in progress: what `SND_NKE`
    /// does.
    pub fn reset(&self) {
        self.arriving
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.giving
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Take the records of one telegram written to the meter; `true` when
    /// the write is complete and what arrived is now held.
    ///
    /// # Errors
    /// Records that are no Stream.
    pub fn take(&self, body: &[u8]) -> Result<bool> {
        let (bytes, more) = record::unpack(body)?;
        let mut arriving = self.arriving.lock().unwrap_or_else(PoisonError::into_inner);
        arriving.extend_from_slice(&bytes);
        if more {
            return Ok(false);
        }
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = std::mem::take(&mut arriving);
        Ok(true)
    }

    /// The records of the next telegram read from the meter, in `room`
    /// bytes: the first call of a read packs what is held, the last hands
    /// out the body with no "more follow".
    ///
    /// # Errors
    /// Room too small for a record.
    pub fn give(&self, room: usize) -> Result<Vec<u8>> {
        let mut giving = self.giving.lock().unwrap_or_else(PoisonError::into_inner);
        if giving.is_empty() {
            giving.extend(record::pack(&self.held(), room)?);
        }
        giving
            .pop_front()
            .ok_or_else(|| protocol_error("a read with nothing to give"))
    }

    /// The access number of the next answer: one more each time.
    #[must_use]
    pub fn next_access(&self) -> u8 {
        self.access.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        Identity {
            ident: 12_345_678,
            manufacturer: *b"XMP",
            version: 1,
            medium: 7,
        }
    }

    #[test]
    fn an_identity_packs_into_the_headers_and_the_link_address() {
        let identity = identity();
        assert_eq!(identity.manufacturer_code(), 0x61b0, "X=24 M=13 P=16");
        assert_eq!(identity.ident_bcd(), [0x78, 0x56, 0x34, 0x12]);
        assert_eq!(
            identity.link_address(),
            [0xb0, 0x61, 0x78, 0x56, 0x34, 0x12, 1, 7]
        );
        assert_eq!(
            identity.long_header(3),
            [0x78, 0x56, 0x34, 0x12, 0xb0, 0x61, 1, 7, 3, 0, 0, 0]
        );
        assert_eq!(Identity::short_header(9), [9, 0, 0, 0]);
        assert_eq!(identity.label(), "XMP-12345678");
    }

    #[test]
    fn a_meter_takes_a_write_over_telegrams_and_gives_it_back_over_telegrams() {
        let meter = Meter::new(identity()).at_primary(1);
        assert!(meter.answers_to(1) && meter.answers_to(0xfe) && meter.answers_to(0xff));
        assert!(!meter.answers_to(2));
        let payload: Vec<u8> = (0..500u32)
            .map(|n| u8::try_from(n % 251).unwrap_or(0))
            .collect();
        let bodies = record::pack(&payload, 252).expect("pack");
        for body in &bodies[..bodies.len() - 1] {
            assert!(!meter.take(body).expect("take"), "more follow");
            assert!(meter.held().is_empty(), "not held until complete");
        }
        assert!(meter.take(&bodies[bodies.len() - 1]).expect("take"));
        assert_eq!(meter.held(), payload);
        let mut back = Vec::new();
        loop {
            let (bytes, more) = record::unpack(&meter.give(240).expect("give")).expect("unpack");
            back.extend_from_slice(&bytes);
            if !more {
                break;
            }
        }
        assert_eq!(back, payload);
        assert_eq!(meter.next_access(), 1);
        assert_eq!(meter.next_access(), 2);
        assert_eq!(meter.primary(), 1);
        assert_eq!(meter.identity(), &identity());
    }

    #[test]
    fn a_reset_forgets_what_was_in_progress() {
        let meter = Meter::new(identity());
        let bodies = record::pack(&[9; 400], 252).expect("pack");
        assert!(!meter.take(&bodies[0]).expect("take"));
        meter.reset();
        assert!(meter.take(&bodies[1]).expect("take"));
        assert_eq!(meter.held().len(), 400 - 191, "only the last telegram");
        assert!(meter.take(&[0x02, 0x13]).is_err(), "a numeric record");
        assert!(meter.give(3).is_err(), "no room");
    }
}
