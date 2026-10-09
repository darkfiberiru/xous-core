// meetcmd.rs - manages the badge's meeting-link list over USB serial.
//
// The `meet` console command in dc34-console is only a forwarder: it passes the rest of the line to
// this app (see `dc34_api::forward_meet_cmd`) and the replies are printed from here. The logic lives
// in the vault because the vault is loaded from swap, where there is plenty of room, while the
// console is part of the size-constrained RRAM image.
//
// The serial console delivers input one character at a time through the keyboard service, so long
// lines are unreliable. A meeting (`<name>\t<url>`, up to ~1 kB) is therefore staged in small
// base64 chunks and committed with a CRC-32 over the whole payload:
//
//   meet ver                 -> MEET PROTO 1
//   meet begin               -> MEET OK 0            (empties the staging buffer)
//   meet data <base64>       -> MEET OK <bytes>      (appends; at most 48 decoded bytes per line)
//   meet save <crc32 hex>    -> MEET SAVED           (buffer is <name>\t<url>)
//   meet del <crc32 hex>     -> MEET DELETED | MEET MISSING   (buffer is <name>)
//   meet clear               -> MEET CLEARED <count>
//   meet list                -> MEET ITEM <index> <base64 chunk>   (repeated; chunks of one index
//                               concatenate to base64 of <name>\t<url>)
//                               MEET END <count>
//   meet enter [on|off]      -> MEET ENTER on|off    (press Enter after typing; no argument = query)
//
// Every failure is reported as `MEET ERR <reason>`. The host side lives in `tools-bao/dc34-meetings`.
//
// Input only works after "USB serial..." has been enabled from the badge's token menu, the same
// requirement as for the `image` command.

use core::fmt::Write;

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use dc34_api::meeting::*;
use dc34_api::meeting_store;
use pddb::Pddb;

const PROTO_VERSION: u32 = 1;
/// Largest payload that can be staged: a maximum-length name, the separator, and a maximum-length URL.
const STAGE_MAX: usize = MEETING_NAME_MAX + 1 + MEETING_URL_MAX;
/// Largest decoded chunk accepted per `meet data` line.
const CHUNK_MAX: usize = 48;
/// Base64 characters per `MEET ITEM` output line.
const LIST_CHUNK_CHARS: usize = 64;

pub struct MeetCmd {
    stage: Vec<u8>,
    pddb: Pddb,
}

impl MeetCmd {
    pub fn new() -> Self { MeetCmd { stage: Vec::with_capacity(STAGE_MAX), pddb: Pddb::new() } }

    /// Takes the staged payload if `crc_hex` matches it. The buffer is emptied either way, so a failed
    /// transfer can never be completed by a later, unrelated command.
    fn take_staged(&mut self, crc_hex: &str) -> Result<String, &'static str> {
        let staged = core::mem::take(&mut self.stage);
        let expected = u32::from_str_radix(crc_hex.trim(), 16).or(Err("bad crc argument"))?;
        if staged.is_empty() {
            return Err("nothing staged");
        }
        if crc32(&staged) != expected {
            return Err("crc mismatch");
        }
        String::from_utf8(staged).or(Err("not utf-8"))
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

impl MeetCmd {
    /// Runs one `meet ...` command line (without the leading `meet`). Returns the reply line, and
    /// whether the stored list changed so that the caller can refresh an open Meetings screen.
    pub fn process(&mut self, args: &str) -> (String, bool) {
        let mut ret = String::new();
        let mut changed = false;
        let args = args.trim();
        let (sub, rest) = match args.split_once(' ') {
            Some((sub, rest)) => (sub, rest.trim()),
            None => (args, ""),
        };
        match sub {
            "ver" => write!(ret, "MEET PROTO {}", PROTO_VERSION).unwrap(),
            "begin" => {
                self.stage.clear();
                write!(ret, "MEET OK 0").unwrap();
            }
            "data" => match B64.decode(rest) {
                Ok(chunk) if !chunk.is_empty() && chunk.len() <= CHUNK_MAX => {
                    if self.stage.len() + chunk.len() > STAGE_MAX {
                        self.stage.clear();
                        write!(ret, "MEET ERR too long").unwrap();
                    } else {
                        self.stage.extend_from_slice(&chunk);
                        write!(ret, "MEET OK {}", self.stage.len()).unwrap();
                    }
                }
                _ => {
                    self.stage.clear();
                    write!(ret, "MEET ERR bad chunk").unwrap();
                }
            },
            "save" => {
                let result = self
                    .take_staged(rest)
                    .map_err(|e| e.to_string())
                    .and_then(|s| MeetingRecord::from_serial(&s, Self::now()).map_err(|e| e.to_string()))
                    .and_then(|rec| {
                        meeting_store::save_meeting(&self.pddb, &rec).map_err(|e| format!("storage: {}", e))
                    });
                match result {
                    Ok(()) => {
                        changed = true;
                        write!(ret, "MEET SAVED").unwrap();
                    }
                    Err(e) => write!(ret, "MEET ERR {}", e).unwrap(),
                }
            }
            "del" => {
                let result = self.take_staged(rest).map_err(|e| e.to_string()).and_then(|name| {
                    meeting_store::delete_meeting(&self.pddb, &name).map_err(|e| format!("storage: {}", e))
                });
                match result {
                    Ok(true) => {
                        changed = true;
                        write!(ret, "MEET DELETED").unwrap();
                    }
                    Ok(false) => write!(ret, "MEET MISSING").unwrap(),
                    Err(e) => write!(ret, "MEET ERR {}", e).unwrap(),
                }
            }
            "clear" => match meeting_store::clear_meetings(&self.pddb) {
                Ok(count) => {
                    changed = true;
                    write!(ret, "MEET CLEARED {}", count).unwrap();
                }
                Err(e) => write!(ret, "MEET ERR storage: {}", e).unwrap(),
            },
            "list" => match meeting_store::load_meetings(&self.pddb) {
                Ok((meetings, _skipped)) => {
                    for (index, rec) in meetings.iter().enumerate() {
                        let encoded = B64.encode(rec.to_serial().as_bytes());
                        // base64 is pure ASCII, so slicing on byte offsets is safe
                        for chunk in encoded.as_bytes().chunks(LIST_CHUNK_CHARS) {
                            println!("MEET ITEM {} {}", index, core::str::from_utf8(chunk).unwrap());
                        }
                    }
                    write!(ret, "MEET END {}", meetings.len()).unwrap();
                }
                Err(e) => write!(ret, "MEET ERR storage: {}", e).unwrap(),
            },
            "enter" => {
                let outcome = match rest {
                    "on" => meeting_store::set_meeting_enter(&self.pddb, true),
                    "off" => meeting_store::set_meeting_enter(&self.pddb, false),
                    "" => Ok(()),
                    _ => Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "use on or off")),
                };
                match outcome {
                    Ok(()) => write!(
                        ret,
                        "MEET ENTER {}",
                        if meeting_store::get_meeting_enter(&self.pddb) { "on" } else { "off" }
                    )
                    .unwrap(),
                    Err(e) => write!(ret, "MEET ERR {}", e).unwrap(),
                }
            }
            _ => write!(ret, "MEET ERR usage: meet ver|begin|data|save|del|clear|list|enter").unwrap(),
        }
        (ret, changed)
    }
}
