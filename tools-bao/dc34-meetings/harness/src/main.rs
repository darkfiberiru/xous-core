//! Speaks the badge's serial console protocol on stdin/stdout using the real firmware code.
//!
//! Mimics what the host sees from a badge: every command is echoed as `[console] <line>` before the
//! reply, and unknown verbs get the command list. `HARNESS_BULK_LIMIT_TEST`-style knobs are kept out
//! on purpose - this should behave like the device, nothing more.

use std::io::{BufRead, Write};

#[path = "../../../../apps-baosec/dc34-vault/src/meetcmd.rs"]
mod meetcmd;

fn main() {
    let mut meet = meetcmd::MeetCmd::new();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.expect("stdin");
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            continue;
        }
        println!("[console] {}", line);
        // some unrelated log chatter, as a real badge produces
        println!("INFO:dc34_console: heartbeat");
        if let Some(args) = line.strip_prefix("meet") {
            let (reply, _changed) = meet.process(args);
            println!("{}", reply);
        } else if let Some(raw) = line.strip_prefix("corrupt ") {
            // test hook, not a badge command: plants an unparseable record
            pddb::Pddb::raw_put(dc34_api::meeting::MEETING_DICT, raw, b"not a record");
            println!("OK");
        } else {
            println!("Commands: echo, ver, test, image, bio, meet");
        }
        out.flush().ok();
    }
}
