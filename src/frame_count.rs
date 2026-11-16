//! The frame count bit of each meter a line reaches (EN 13757-2, the link
//! layer: the bit a master toggles between its calls to one meter, so a
//! meter that sees it unchanged takes the call for a repeat of the last).
//!
//! It is the meter's, by its primary address, not the Location's: until
//! 2026-11-15 each Location kept one bit for every meter it called, so two
//! Locations reaching one meter toggled it apart and the meter took the
//! second's telegram for a repeat of the first's (open problem 24). On a
//! port the Locations of a node share, the bits are kept with the port,
//! under its path in what they share (`Configured::shared_by`), so every
//! Location reaching a meter alternates one bit; on a line nothing is
//! shared on, they are the Location's own, kept by its clones. The crate
//! addresses a meter by its primary address alone, so that is the key.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use xcore::lock::Locked;

/// The frame count bit of each meter a line reaches, by primary address.
#[derive(Default)]
pub(crate) struct FrameCounts(Mutex<BTreeMap<u8, Arc<Mutex<bool>>>>);

impl FrameCounts {
    /// The bit of the meter at `address`: set, until a call to it toggles
    /// it. Held across a call, so two Locations calling the meter at once
    /// each carry the bit the other left.
    pub(crate) fn of(&self, address: u8) -> Arc<Mutex<bool>> {
        Arc::clone(
            self.0
                .locked()
                .entry(address)
                .or_insert_with(|| Arc::new(Mutex::new(true))),
        )
    }
}
