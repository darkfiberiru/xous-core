//! Meeting-link records: a named URL that the badge can type into a host computer as a USB keyboard.
//!
//! This file is deliberately free of any Xous dependencies so that the parsing and validation logic
//! can be unit-tested on a host. The PDDB glue lives in `meeting_store.rs`.
//!
//! # Storage format
//!
//! One PDDB key per meeting, in the `vault.meetings` dictionary. The key name is the meeting name; the
//! value follows the same `tag:value\n` text convention as the password and TOTP records:
//!
//! ```text
//! version:1
//! name:Weekly sync
//! url:https://example.com/j/123456789?pwd=abc
//! ctime:1791500000
//! atime:0
//! count:0
//! ```
//!
//! # QR format
//!
//! `meeting://<name>|<url>` - the name ends at the first `|`. Nothing is percent-encoded, which keeps
//! the QR code as small as possible for the badge's camera. A QR code is unauthenticated input, so
//! the URL in one must start with `http://` or `https://`; other schemes can only be loaded over USB.
//!
//! # Serial transfer format
//!
//! `<name>\t<url>` as UTF-8, sent in chunks and protected by a CRC-32. See `apps-baosec/dc34-vault`
//! `src/meetcmd.rs` for the wire protocol.

/// PDDB dictionary holding one key per meeting.
pub const MEETING_DICT: &str = "vault.meetings";
/// Key inside `DC34_DICT` holding the "press Enter after the URL" setting (one byte: 0 or 1).
pub const DC34_MEETING_ENTER: &str = "meeting.enter";
/// Opcode of the vault app message that carries one `meet` console command line (a `MeetCmdIpc`).
/// Explicitly numbered because it crosses a process boundary, in the same style as `ImageLoad = 1024`.
pub const MEETING_CMD_OP: usize = 1029;

pub const MEETING_REC_VERSION: u32 = 1;
/// Maximum length of a meeting name in bytes. Bounded by the PDDB key name limit (95 bytes), and kept
/// short because the display is 128 pixels wide.
pub const MEETING_NAME_MAX: usize = 48;
/// Maximum length of a URL in bytes. Calendar-generated links can be several hundred characters.
pub const MEETING_URL_MAX: usize = 1024;
/// Scheme used by QR codes that add a meeting.
pub const MEETING_QR_SCHEME: &str = "meeting";
/// Separator between name and URL in a QR payload.
pub const MEETING_QR_SEP: char = '|';
/// Separator between name and URL in a serial transfer.
pub const MEETING_SERIAL_SEP: char = '\t';
/// By default the badge presses Enter after typing the URL.
pub const MEETING_ENTER_DEFAULT: bool = true;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MeetingError {
    /// Name is empty after trimming
    EmptyName,
    /// Name is longer than `MEETING_NAME_MAX` bytes
    NameTooLong,
    /// Name contains a control character, or one of the reserved separators `:` and `|`
    BadNameChar,
    /// URL is empty
    EmptyUrl,
    /// URL is longer than `MEETING_URL_MAX` bytes
    UrlTooLong,
    /// URL contains something a USB keyboard can't type: whitespace, control or non-ASCII characters
    BadUrlChar,
    /// A QR code's URL does not start with `http://` or `https://`
    NotWebLink,
    /// The name/URL separator is missing
    MissingSeparator,
    /// A stored record could not be parsed
    Malformed,
}

impl core::fmt::Display for MeetingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            MeetingError::EmptyName => "name is empty",
            MeetingError::NameTooLong => "name is too long (48 bytes max)",
            MeetingError::BadNameChar => "name has an illegal character (no ':' '|' or control chars)",
            MeetingError::EmptyUrl => "URL is empty",
            MeetingError::UrlTooLong => "URL is too long (1024 bytes max)",
            MeetingError::BadUrlChar => "URL has a character a keyboard can't type (space or non-ASCII)",
            MeetingError::NotWebLink => "a QR code link must start with http:// or https://",
            MeetingError::MissingSeparator => "missing separator between name and URL",
            MeetingError::Malformed => "stored record is malformed",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MeetingRecord {
    pub version: u32,
    /// Display name; also the PDDB key name, so it is unique.
    pub name: String,
    /// What gets typed. Printable ASCII only.
    pub url: String,
    pub ctime: u64,
    pub atime: u64,
    pub count: u64,
}

/// Trims the name and checks it is usable as both a display string and a PDDB key name.
pub fn validate_name(name: &str) -> Result<&str, MeetingError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(MeetingError::EmptyName);
    }
    if name.len() > MEETING_NAME_MAX {
        return Err(MeetingError::NameTooLong);
    }
    // ':' is the PDDB path separator and the record's tag delimiter; '|' is the QR separator.
    if name.chars().any(|c| c.is_control() || c == ':' || c == MEETING_QR_SEP) {
        return Err(MeetingError::BadNameChar);
    }
    Ok(name)
}

/// Trims the URL and checks that every character can be typed by the USB keyboard emulation.
///
/// No particular scheme is required, so that app links (`zoommtg://`, `msteams:`) and bare meeting
/// IDs also work.
pub fn validate_url(url: &str) -> Result<&str, MeetingError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(MeetingError::EmptyUrl);
    }
    if url.len() > MEETING_URL_MAX {
        return Err(MeetingError::UrlTooLong);
    }
    // printable ASCII excluding space: 0x21..=0x7E
    if !url.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(MeetingError::BadUrlChar);
    }
    Ok(url)
}

impl MeetingRecord {
    /// Builds a validated record. `ctime` is seconds since the Unix epoch (0 if the clock is not set).
    pub fn new(name: &str, url: &str, ctime: u64) -> Result<Self, MeetingError> {
        let name = validate_name(name)?;
        let url = validate_url(url)?;
        Ok(MeetingRecord {
            version: MEETING_REC_VERSION,
            name: name.to_owned(),
            url: url.to_owned(),
            ctime,
            atime: 0,
            count: 0,
        })
    }

    /// Parses the part of a QR code after `meeting://`, i.e. `<name>|<url>`.
    pub fn from_qr(data: &str, ctime: u64) -> Result<Self, MeetingError> {
        let (name, url) = data.split_once(MEETING_QR_SEP).ok_or(MeetingError::MissingSeparator)?;
        let rec = Self::new(name, url, ctime)?;
        // Anyone can show the badge a QR code, so keep what one can store to things that look like
        // web links. This is a plausibility check, not a guarantee of what the text does when typed -
        // the on-badge approval of the full link is what the user relies on.
        let lower = rec.url.to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://")) {
            return Err(MeetingError::NotWebLink);
        }
        Ok(rec)
    }

    /// Parses a serial transfer payload, i.e. `<name>\t<url>`.
    pub fn from_serial(data: &str, ctime: u64) -> Result<Self, MeetingError> {
        let (name, url) = data.split_once(MEETING_SERIAL_SEP).ok_or(MeetingError::MissingSeparator)?;
        Self::new(name, url, ctime)
    }

    /// The inverse of `from_serial`.
    pub fn to_serial(&self) -> String { format!("{}{}{}", self.name, MEETING_SERIAL_SEP, self.url) }

    /// The PDDB key name for this record.
    pub fn key(&self) -> &str { &self.name }

    /// The exact string sent to the host keyboard.
    pub fn typed(&self, press_enter: bool) -> String {
        let mut s = String::with_capacity(self.url.len() + 1);
        s.push_str(&self.url);
        if press_enter {
            // the USB HID mapping turns LF into the Enter key
            s.push('\n');
        }
        s
    }

    pub fn to_vec(&self) -> Vec<u8> {
        format!(
            "version:{}\nname:{}\nurl:{}\nctime:{}\natime:{}\ncount:{}\n",
            self.version, self.name, self.url, self.ctime, self.atime, self.count
        )
        .into_bytes()
    }

    /// Parses a stored record. The result is re-validated, so a corrupted record can never cause
    /// unexpected keystrokes to be sent to the host.
    pub fn from_slice(data: &[u8]) -> Result<Self, MeetingError> {
        let text = core::str::from_utf8(data).or(Err(MeetingError::Malformed))?;
        let mut rec = MeetingRecord::default();
        let mut have_version = false;
        for line in text.split('\n') {
            // only the first ':' is a delimiter - the URL itself contains more of them
            if let Some((tag, value)) = line.split_once(':') {
                match tag {
                    "version" => {
                        rec.version = value.parse().or(Err(MeetingError::Malformed))?;
                        have_version = true;
                    }
                    "name" => rec.name = value.to_owned(),
                    "url" => rec.url = value.to_owned(),
                    "ctime" => rec.ctime = value.parse().or(Err(MeetingError::Malformed))?,
                    "atime" => rec.atime = value.parse().or(Err(MeetingError::Malformed))?,
                    "count" => rec.count = value.parse().or(Err(MeetingError::Malformed))?,
                    // ignore unknown tags so newer records still load on older firmware
                    _ => {}
                }
            }
        }
        if !have_version {
            return Err(MeetingError::Malformed);
        }
        if validate_name(&rec.name)? != rec.name || validate_url(&rec.url)? != rec.url {
            return Err(MeetingError::Malformed);
        }
        Ok(rec)
    }
}

/// CRC-32 (IEEE, the same polynomial as zlib / `crc32fast` / Python's `zlib.crc32`), implemented here
/// so that the check does not depend on which crates the caller happens to link.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (!(crc & 1)).wrapping_add(1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.zoom.us/j/1234567890?pwd=aBc:dEf%20_-~";

    #[test]
    fn qr_roundtrip() {
        let r = MeetingRecord::from_qr(&format!("Weekly sync|{}", URL), 42).unwrap();
        assert_eq!(r.name, "Weekly sync");
        assert_eq!(r.url, URL);
        assert_eq!(r.ctime, 42);
        assert_eq!(r.version, MEETING_REC_VERSION);
    }

    #[test]
    fn qr_url_may_contain_separator() {
        // only the first '|' splits; later ones belong to the URL
        let r = MeetingRecord::from_qr("a|https://x.test/?q=1|2", 0).unwrap();
        assert_eq!(r.name, "a");
        assert_eq!(r.url, "https://x.test/?q=1|2");
    }

    #[test]
    fn storage_roundtrip() {
        let mut r = MeetingRecord::new("  Standup  ", URL, 7).unwrap();
        assert_eq!(r.name, "Standup");
        r.atime = 99;
        r.count = 3;
        let back = MeetingRecord::from_slice(&r.to_vec()).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn serial_roundtrip() {
        let r = MeetingRecord::new("Standup", URL, 0).unwrap();
        assert_eq!(MeetingRecord::from_serial(&r.to_serial(), 0).unwrap(), r);
    }

    #[test]
    fn typed_string() {
        let r = MeetingRecord::new("x", "https://a.test/b", 0).unwrap();
        assert_eq!(r.typed(true), "https://a.test/b\n");
        assert_eq!(r.typed(false), "https://a.test/b");
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(MeetingRecord::new("", URL, 0), Err(MeetingError::EmptyName));
        assert_eq!(MeetingRecord::new("   ", URL, 0), Err(MeetingError::EmptyName));
        assert_eq!(MeetingRecord::new("a:b", URL, 0), Err(MeetingError::BadNameChar));
        assert_eq!(MeetingRecord::new("a|b", URL, 0), Err(MeetingError::BadNameChar));
        assert_eq!(MeetingRecord::new("a\nb", URL, 0), Err(MeetingError::BadNameChar));
        assert_eq!(MeetingRecord::new(&"n".repeat(49), URL, 0), Err(MeetingError::NameTooLong));
        assert!(MeetingRecord::new(&"n".repeat(48), URL, 0).is_ok());
        assert_eq!(MeetingRecord::new("a", "", 0), Err(MeetingError::EmptyUrl));
        assert_eq!(MeetingRecord::new("a", "https://x.test/a b", 0), Err(MeetingError::BadUrlChar));
        assert_eq!(MeetingRecord::new("a", "https://x.test/\u{e9}", 0), Err(MeetingError::BadUrlChar));
        // an embedded newline would otherwise let a QR code type a second line into the host
        assert_eq!(MeetingRecord::new("a", "https://x.test/\ncalc", 0), Err(MeetingError::BadUrlChar));
        assert_eq!(MeetingRecord::new("a", &"u".repeat(1025), 0), Err(MeetingError::UrlTooLong));
        assert!(MeetingRecord::new("a", &"u".repeat(1024), 0).is_ok());
        assert_eq!(MeetingRecord::from_qr("no separator", 0), Err(MeetingError::MissingSeparator));
        // QR codes may only carry web links; other schemes and bare commands need the USB path
        assert_eq!(MeetingRecord::from_qr("a|zoommtg://zoom.us/join?confno=1", 0), Err(MeetingError::NotWebLink));
        assert_eq!(MeetingRecord::from_qr("a|curl$IFS$9evil.test", 0), Err(MeetingError::NotWebLink));
        assert!(MeetingRecord::from_qr("a|HTTPS://x.test/", 0).is_ok());
        assert!(MeetingRecord::from_serial("a\tzoommtg://zoom.us/join?confno=1", 0).is_ok());
        assert_eq!(MeetingRecord::from_serial("a|b", 0), Err(MeetingError::MissingSeparator));
    }

    #[test]
    fn non_ascii_names_are_fine() {
        let r = MeetingRecord::new("Réunion équipe", URL, 0).unwrap();
        assert_eq!(MeetingRecord::from_slice(&r.to_vec()).unwrap(), r);
    }

    #[test]
    fn rejects_corrupt_records() {
        assert_eq!(MeetingRecord::from_slice(b"\xff\xfe"), Err(MeetingError::Malformed));
        assert_eq!(MeetingRecord::from_slice(b""), Err(MeetingError::Malformed));
        assert_eq!(MeetingRecord::from_slice(b"name:a\nurl:b\n"), Err(MeetingError::Malformed));
        assert_eq!(MeetingRecord::from_slice(b"version:x\nname:a\nurl:b\n"), Err(MeetingError::Malformed));
        assert_eq!(MeetingRecord::from_slice(b"version:1\nname:a\n"), Err(MeetingError::EmptyUrl));
        // a stored URL with a space must not be typed
        assert_eq!(MeetingRecord::from_slice(b"version:1\nname:a\nurl:b c\n"), Err(MeetingError::BadUrlChar));
        // unknown tags are tolerated
        assert!(MeetingRecord::from_slice(b"version:1\nname:a\nurl:b\nfuture:1\n").is_ok());
    }

    #[test]
    fn crc32_matches_zlib() {
        // reference values from Python: zlib.crc32(b"...")
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"Standup\thttps://a.test/b"), 0xE23F_5870);
    }
}
