//! PDDB glue for meeting records. Shared by the vault app (which shows and types them) and the
//! console service (which loads them over USB serial), so both always agree on the on-disk format.

use std::io::{Error, ErrorKind, Read, Result, Write};

use pddb::Pddb;

use crate::DC34_DICT;
use crate::meeting::*;

/// Allocation hint for a new record. Typical links fit; a maximum-length one is about 1.1 kB and
/// simply grows past the hint.
const MEETING_ALLOC_HINT: usize = 256;
/// `delete_key_list` packs the names into one message page, so bulk deletes are done in batches that
/// fit even when every name is at the maximum length.
const DELETE_BATCH: usize = 32;
/// Bulk-read budget. A full list of maximum-length records will not fit, in which case the oversized
/// records are fetched one at a time.
const MEETING_BULK_LIMIT: usize = 64 * 1024;

fn invalid(e: MeetingError) -> Error { Error::new(ErrorKind::InvalidData, e.to_string()) }

/// Writes `rec`, replacing any existing meeting of the same name.
pub fn save_meeting(pddb: &Pddb, rec: &MeetingRecord) -> Result<()> {
    // Delete first: writing a shorter value over a longer one would leave the old tail behind.
    match pddb.delete_key(MEETING_DICT, rec.key(), None) {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut key =
        pddb.get(MEETING_DICT, rec.key(), None, true, true, Some(MEETING_ALLOC_HINT), None::<fn()>)?;
    key.write_all(&rec.to_vec())?;
    drop(key);
    pddb.sync()
}

/// Reads one meeting by name.
pub fn load_meeting(pddb: &Pddb, name: &str) -> Result<MeetingRecord> {
    let mut key = pddb.get(MEETING_DICT, name, None, false, false, None, None::<fn()>)?;
    let mut data = Vec::new();
    key.read_to_end(&mut data)?;
    MeetingRecord::from_slice(&data).map_err(invalid)
}

/// Reads every meeting. Records that fail to parse are skipped and counted in the second return value,
/// so one corrupt entry can't hide the rest of the list.
pub fn load_meetings(pddb: &Pddb) -> Result<(Vec<MeetingRecord>, usize)> {
    let keys = match pddb.read_dict(MEETING_DICT, None, Some(MEETING_BULK_LIMIT)) {
        Ok(keys) => keys,
        // no dictionary yet just means no meetings have been added
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(e),
    };
    let mut meetings = Vec::with_capacity(keys.len());
    let mut skipped = 0;
    for key in keys {
        let parsed = match key.data {
            Some(data) => MeetingRecord::from_slice(&data).map_err(invalid),
            // didn't fit in the bulk read; fetch it individually
            None => load_meeting(pddb, &key.name),
        };
        match parsed {
            // the key name is authoritative; a record stored under a different name is corrupt
            Ok(rec) if rec.name == key.name => meetings.push(rec),
            _ => {
                log::warn!("skipping unreadable meeting record {}", key.name);
                skipped += 1;
            }
        }
    }
    meetings.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok((meetings, skipped))
}

/// Whether a key of this name exists, readable or not.
pub fn meeting_exists(pddb: &Pddb, name: &str) -> bool {
    pddb.get(MEETING_DICT, name, None, false, false, None, None::<fn()>).is_ok()
}

/// Deletes one meeting. Returns `Ok(false)` if there was no meeting of that name.
pub fn delete_meeting(pddb: &Pddb, name: &str) -> Result<bool> {
    match pddb.delete_key(MEETING_DICT, name, None) {
        Ok(_) => {
            pddb.sync()?;
            Ok(true)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Deletes every meeting. Returns how many there were.
pub fn clear_meetings(pddb: &Pddb) -> Result<usize> {
    let names = match pddb.list_keys(MEETING_DICT, None) {
        Ok(names) => names,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let count = names.len();
    for batch in names.chunks(DELETE_BATCH) {
        pddb.delete_key_list(MEETING_DICT, batch.to_vec(), None)?;
    }
    if count > 0 {
        pddb.sync()?;
    }
    Ok(count)
}

/// Records that a meeting was typed: bumps the use count and access time.
pub fn touch_meeting(pddb: &Pddb, rec: &mut MeetingRecord, now: u64) -> Result<()> {
    rec.count = rec.count.saturating_add(1);
    rec.atime = now;
    save_meeting(pddb, rec)
}

/// Whether Enter is pressed after the URL. Defaults to on when the setting has never been written.
pub fn get_meeting_enter(pddb: &Pddb) -> bool {
    match pddb.get(DC34_DICT, DC34_MEETING_ENTER, None, false, false, None, None::<fn()>) {
        Ok(mut key) => {
            let mut buf = [0u8; 1];
            match key.read(&mut buf) {
                Ok(1) => buf[0] != 0,
                _ => MEETING_ENTER_DEFAULT,
            }
        }
        Err(_) => MEETING_ENTER_DEFAULT,
    }
}

pub fn set_meeting_enter(pddb: &Pddb, enter: bool) -> Result<()> {
    let mut key = pddb.get(DC34_DICT, DC34_MEETING_ENTER, None, true, true, Some(1), None::<fn()>)?;
    key.write_all(&[enter as u8])?;
    drop(key);
    pddb.sync()
}
