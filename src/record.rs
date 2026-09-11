//! The variable data records of EN 13757-3: how a Stream rides in a meter's
//! telegrams, and the control information that says it does.
//!
//! A meter's user data is records, each a data information field, a value
//! information field and the data. A Stream is carried as variable-length
//! records — DIF `0D`, a manufacturer-specific VIF, then one byte of length
//! and that many bytes — at most 191 bytes each, the most the length byte
//! names as text. A telegram holds as many records as fit its room; when
//! more follow in the next telegram, the special DIF `1F` closes this one.
//! Wired and wireless M-Bus share all of this; only the frame around it
//! differs.

use transport::error::{Result, protocol_error};

/// DIF: the data is variable length, its length in the byte after the VIF.
pub const VARIABLE_LENGTH: u8 = 0x0d;
/// VIF: the data is manufacturer specific; a Stream is.
pub const MANUFACTURER_SPECIFIC: u8 = 0x7f;
/// DIF: more records follow in the next telegram.
pub const MORE_FOLLOW: u8 = 0x1f;
/// The most one record holds: the largest length byte that means bytes.
pub const MAX_RECORD: usize = 0xbf;
/// A record's DIF, VIF and length byte.
pub const RECORD_OVERHEAD: usize = 3;

/// Control information: data sent by the master to the meter, no header.
pub const CI_DATA_SEND: u8 = 0x51;
/// Control information: variable data with the twelve-byte long header.
pub const CI_VARIABLE_LONG: u8 = 0x72;
/// Control information: variable data with the four-byte short header.
pub const CI_VARIABLE_SHORT: u8 = 0x7a;

/// The least room a telegram needs: one record of one byte, and the
/// marker that more follow.
pub const MIN_ROOM: usize = RECORD_OVERHEAD + 2;

/// `payload` as telegram bodies of at most `room` bytes each, every one
/// but the last closing with [`MORE_FOLLOW`]. An empty payload is one
/// record of no bytes.
///
/// # Errors
/// Room under [`MIN_ROOM`].
pub fn pack(payload: &[u8], room: usize) -> Result<Vec<Vec<u8>>> {
    if room < MIN_ROOM {
        return Err(protocol_error(format!(
            "{room} bytes of room holds no record"
        )));
    }
    let record = MAX_RECORD.min(room - RECORD_OVERHEAD - 1);
    let chunks: Vec<&[u8]> = if payload.is_empty() {
        vec![&[]]
    } else {
        payload.chunks(record).collect()
    };
    let mut bodies = Vec::new();
    let mut body = Vec::new();
    for chunk in chunks {
        if body.len() + RECORD_OVERHEAD + chunk.len() + 1 > room {
            body.push(MORE_FOLLOW);
            bodies.push(std::mem::take(&mut body));
        }
        body.push(VARIABLE_LENGTH);
        body.push(MANUFACTURER_SPECIFIC);
        body.push(u8::try_from(chunk.len()).unwrap_or(u8::MAX));
        body.extend_from_slice(chunk);
    }
    bodies.push(body);
    Ok(bodies)
}

/// The bytes the records of one telegram body carry, and whether more
/// follow in the next.
///
/// # Errors
/// A record that is not variable length, a length byte that does not mean
/// bytes, a record cut short, or a "more follow" that is not last.
pub fn unpack(body: &[u8]) -> Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    let mut rest = body;
    while let Some((&dif, after)) = rest.split_first() {
        if dif == MORE_FOLLOW {
            if !after.is_empty() {
                return Err(protocol_error(
                    "records after the one that says more follow",
                ));
            }
            return Ok((bytes, true));
        }
        if dif != VARIABLE_LENGTH {
            return Err(protocol_error(format!(
                "a record of DIF {dif:#04x} is no Stream"
            )));
        }
        let [_vif, length, data @ ..] = after else {
            return Err(protocol_error("a record cut short before its length"));
        };
        let length = usize::from(*length);
        if length > MAX_RECORD {
            return Err(protocol_error("a length byte that does not mean bytes"));
        }
        let (chunk, remaining) = data
            .split_at_checked(length)
            .ok_or_else(|| protocol_error("a record cut short before its end"))?;
        bytes.extend_from_slice(chunk);
        rest = remaining;
    }
    Ok((bytes, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_packs_into_records_and_unpacks_whole() {
        let payload: Vec<u8> = (0..600u32)
            .map(|n| u8::try_from(n % 256).unwrap_or(0))
            .collect();
        let bodies = pack(&payload, 252).expect("pack");
        assert_eq!(
            bodies.len(),
            3,
            "191 a record; the 27-byte fourth fits the third body"
        );
        assert!(
            bodies[..2]
                .iter()
                .all(|body| body.last() == Some(&MORE_FOLLOW))
        );
        assert_ne!(bodies[2].last(), Some(&MORE_FOLLOW));
        assert!(bodies.iter().all(|body| body.len() <= 252));
        let mut back = Vec::new();
        for (at, body) in bodies.iter().enumerate() {
            let (bytes, more) = unpack(body).expect("unpack");
            back.extend_from_slice(&bytes);
            assert_eq!(more, at < 2, "body {at}");
        }
        assert_eq!(back, payload);
    }

    #[test]
    fn an_empty_stream_is_one_empty_record_and_small_room_shrinks_the_record() {
        assert_eq!(pack(&[], 252).expect("pack"), vec![vec![0x0d, 0x7f, 0x00]]);
        assert_eq!(
            unpack(&[0x0d, 0x7f, 0x00]).expect("unpack"),
            (vec![], false)
        );
        let bodies = pack(&[1, 2, 3, 4, 5], 6).expect("tight");
        assert_eq!(bodies.len(), 3, "two bytes a record, one record a body");
        assert_eq!(bodies[0], [0x0d, 0x7f, 0x02, 1, 2, 0x1f]);
        assert_eq!(bodies[2], [0x0d, 0x7f, 0x01, 5]);
        assert!(pack(&[], 4).is_err(), "no room for a record");
        let bodies = pack(&[7; 300], 400).expect("two records one body");
        assert_eq!(bodies.len(), 1);
        assert_eq!(unpack(&bodies[0]).expect("unpack").0, [7; 300]);
    }

    #[test]
    fn records_that_are_no_stream_are_refused() {
        assert!(
            unpack(&[0x02, 0x13, 0x01, 0x00]).is_err(),
            "a numeric record"
        );
        assert!(unpack(&[0x0d, 0x7f]).is_err(), "no length");
        assert!(unpack(&[0x0d, 0x7f, 0x05, 1, 2]).is_err(), "cut short");
        assert!(
            unpack(&[0x0d, 0x7f, 0xe4, 1, 2, 3, 4]).is_err(),
            "a binary number"
        );
        assert!(
            unpack(&[0x1f, 0x0d, 0x7f, 0x00]).is_err(),
            "more follow, then more"
        );
        assert_eq!(unpack(&[]).expect("nothing"), (vec![], false));
    }
}
