#!/usr/bin/env python3
"""
dc34_meetings.py - manage the meeting-link list on a DC34 badge over USB serial.

The badge must be running firmware with the Meetings feature, be in token mode, and have
"USB serial..." -> "Enable debug console? Yes" selected from its menu (this resets on every boot).

    dc34_meetings.py list
    dc34_meetings.py add "Weekly sync" https://example.com/j/123
    dc34_meetings.py del "Weekly sync"
    dc34_meetings.py push meetings.txt            # add or update every entry in the file
    dc34_meetings.py push meetings.txt --replace  # make the badge match the file exactly
    dc34_meetings.py pull > meetings.txt          # save what is on the badge
    dc34_meetings.py clear
    dc34_meetings.py enter on|off|show            # press Enter after typing a link?
    dc34_meetings.py check meetings.txt           # validate a file without touching the badge
    dc34_meetings.py qr "Weekly sync" https://example.com/j/123   # text to put in a QR code

A meetings file has one `Name | URL` per line. Blank lines and lines starting with # are ignored.

The wire protocol is documented in apps-baosec/dc34-vault/src/meetcmd.rs.
"""

from __future__ import annotations

import argparse
import base64
import os
import re
import sys
import time
import urllib.parse
import zlib
from typing import Iterable, Optional

NAME_MAX = 48        # bytes; must match MEETING_NAME_MAX in libs/dc34-api/src/meeting.rs
URL_MAX = 1024       # bytes; must match MEETING_URL_MAX
CHUNK = 48           # decoded bytes per `meet data` line; must not exceed CHUNK_MAX in meetcmd.rs
PROTO_VERSION = 1
BAUD_RATE = 1_000_000
KNOWN_USB_IDS = {(0x1D50, 0x6198)}
ENV_PORT_VAR = "DC34_MEETINGS_PORT"
CONSOLE_ECHO = "[console]"
REPLY_PREFIX = "MEET "


class MeetingError(Exception):
    """A problem the user can fix: bad input, badge not ready, transfer failed."""


# -- Unicode links ---------------------------------------------------------------------------------
#
# The badge types with a US-layout USB keyboard, so it can only send printable ASCII. Every Unicode
# URL has an ASCII spelling that browsers treat as the same address: punycode for the host name and
# percent-encoded UTF-8 everywhere else. Links are converted to that spelling here, before transfer;
# the badge itself stays strict and refuses anything it could not type.

# Python's "idna" codec implements IDNA 2003, which maps these characters differently from current
# browsers (e.g. it turns ß into ss). Converting them here could silently point at a different site.
_IDNA_DEVIATIONS = "\u00df\u03c2\u200c\u200d"


def _percent_encode_non_ascii(text: str) -> str:
    return re.sub(r"[^\x00-\x7f]", lambda m: "".join(f"%{b:02X}" for b in m.group().encode("utf-8")), text)


def to_ascii_url(url: str) -> str:
    """Return the ASCII spelling of `url`. ASCII input is returned untouched."""
    url = url.strip()
    if url.isascii():
        return url
    parts = urllib.parse.urlsplit(url)
    netloc = parts.netloc
    if netloc and not netloc.isascii():
        userinfo, at, hostport = netloc.rpartition("@")
        host, colon, port = hostport.partition(":")
        if any(c in host for c in _IDNA_DEVIATIONS):
            raise MeetingError(f"the host name {host!r} contains a character that browsers and this tool "
                               "convert differently. Open the link in a browser, copy the address back "
                               "out (it will start with xn--) and use that instead.")
        try:
            host = host.encode("idna").decode("ascii")
        except UnicodeError as e:
            raise MeetingError(f"cannot convert the host name {host!r} to ASCII: {e}") from None
        netloc = _percent_encode_non_ascii(userinfo) + at + host + colon + port
    rest = urllib.parse.urlunsplit(("", "", parts.path, parts.query, parts.fragment))
    prefix = f"{parts.scheme}:" if parts.scheme else ""
    if netloc or url[len(prefix):].startswith("//"):
        prefix += "//" + netloc
    return prefix + _percent_encode_non_ascii(rest)


# -- Validation: the same rules the firmware enforces, so mistakes are caught before any transfer --

def validate(name: str, url: str) -> tuple[str, str]:
    name, url = name.strip(), to_ascii_url(url)
    if not name:
        raise MeetingError("name is empty")
    if len(name.encode("utf-8")) > NAME_MAX:
        raise MeetingError(f"name {name!r} is longer than {NAME_MAX} bytes")
    if any(c in ":|" or ord(c) < 0x20 or 0x7F <= ord(c) < 0xA0 for c in name):
        raise MeetingError(f"name {name!r} contains ':' , '|' or a control character")
    if not url:
        raise MeetingError(f"{name!r}: URL is empty")
    if len(url) > URL_MAX:
        raise MeetingError(f"{name!r}: URL is longer than {URL_MAX} characters")
    if not all(0x21 <= ord(c) <= 0x7E for c in url):
        raise MeetingError(f"{name!r}: URL contains a space or control character, which the badge "
                           "cannot type. Write a space as %20.")
    return name, url


def parse_file(lines: Iterable[str]) -> list[tuple[str, str]]:
    """Parse `Name | URL` lines. Raises MeetingError naming the offending line."""
    meetings: dict[str, tuple[str, str]] = {}
    for number, raw in enumerate(lines, start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if "|" not in line:
            raise MeetingError(f"line {number}: expected `Name | URL`")
        name, url = line.split("|", 1)
        try:
            name, url = validate(name, url)
        except MeetingError as e:
            raise MeetingError(f"line {number}: {e}") from None
        if name in meetings:
            raise MeetingError(f"line {number}: {name!r} appears twice")
        meetings[name] = (name, url)
    return list(meetings.values())


def qr_payload(name: str, url: str) -> str:
    name, url = validate(name, url)
    if not url.lower().startswith(("https://", "http://")):
        raise MeetingError("a QR code link must start with http:// or https://; "
                           "load other kinds of link over USB with `add` or `push`")
    return f"meeting://{name}|{url}"


# -- Device protocol ------------------------------------------------------------------------------

class Badge:
    """Speaks the `meet` console protocol over any object with write(bytes) and readline() -> bytes."""

    def __init__(self, link, timeout: float = 15.0, line_delay: float = 0.0, verbose: bool = False):
        self.link = link
        self.timeout = timeout
        self.line_delay = line_delay
        self.verbose = verbose

    def _command(self, args: str, collect: Optional[list[str]] = None) -> str:
        """Send `meet <args>` and return the text after `MEET ` on the reply line.

        `MEET ITEM` lines are appended to `collect` rather than treated as the reply.
        """
        self.link.write(f"meet {args}\n".encode("ascii"))
        if hasattr(self.link, "flush"):
            self.link.flush()
        deadline = time.monotonic() + self.timeout
        saw_command_list = False
        while time.monotonic() < deadline:
            raw = self.link.readline()
            if not raw:
                continue
            line = raw.decode("utf-8", errors="replace").strip()
            if self.verbose:
                print(f"    < {line}", file=sys.stderr)
            if line.startswith(CONSOLE_ECHO):
                continue  # the console echoes every command before answering
            if "Commands:" in line and "meet" not in line.split("Commands:", 1)[1]:
                saw_command_list = True
                break
            at = line.find(REPLY_PREFIX)
            if at < 0:
                continue  # unrelated log output
            reply = line[at + len(REPLY_PREFIX):]
            if reply.startswith("ITEM ") and collect is not None:
                collect.append(reply[len("ITEM "):])
                deadline = time.monotonic() + self.timeout
                continue
            if self.line_delay:
                time.sleep(self.line_delay)
            return reply
        if saw_command_list:
            raise MeetingError("this badge's firmware has no `meet` command - flash the Meetings firmware first")
        raise MeetingError(
            "no reply from the badge. Check that it is in token mode and that \"USB serial...\" -> "
            "\"Enable debug console? Yes\" has been selected from its menu since it was last powered on.")

    def _expect(self, args: str, prefix: str) -> str:
        reply = self._command(args)
        if not reply.startswith(prefix):
            raise MeetingError(f"badge answered {reply!r} to `meet {args.split(' ')[0]}`")
        return reply[len(prefix):].strip()

    def hello(self) -> None:
        version = self._expect("ver", "PROTO")
        if version != str(PROTO_VERSION):
            raise MeetingError(f"badge speaks protocol {version}, this tool speaks {PROTO_VERSION}; "
                               "update whichever is older")

    def _stage(self, payload: bytes) -> str:
        """Upload `payload` to the badge's staging buffer and return its CRC as the commit token."""
        self._expect("begin", "OK")
        for offset in range(0, len(payload), CHUNK):
            chunk = payload[offset:offset + CHUNK]
            staged = self._expect("data " + base64.b64encode(chunk).decode("ascii"), "OK")
            if staged != str(offset + len(chunk)):
                raise MeetingError(f"badge has {staged} bytes staged, expected {offset + len(chunk)}")
        return f"{zlib.crc32(payload) & 0xFFFFFFFF:08x}"

    def _with_retry(self, what: str, action, attempts: int = 3):
        last: Optional[MeetingError] = None
        for _ in range(attempts):
            try:
                return action()
            except MeetingError as e:
                # a dropped character shows up as a CRC or chunk error; the transfer is restartable
                last = e
                if "no `meet` command" in str(e) or "no reply" in str(e):
                    break
        raise MeetingError(f"{what}: {last}")

    def add(self, name: str, url: str) -> None:
        name, url = validate(name, url)
        payload = f"{name}\t{url}".encode("utf-8")
        self._with_retry(f"saving {name!r}", lambda: self._expect("save " + self._stage(payload), "SAVED"))

    def delete(self, name: str) -> bool:
        """Returns False if the badge had no meeting of that name."""
        payload = name.strip().encode("utf-8")
        if not payload:
            raise MeetingError("name is empty")

        def attempt() -> bool:
            reply = self._command("del " + self._stage(payload))
            if reply.startswith("DELETED"):
                return True
            if reply.startswith("MISSING"):
                return False
            raise MeetingError(f"badge answered {reply!r}")

        return self._with_retry(f"deleting {name!r}", attempt)

    def clear(self) -> int:
        return int(self._expect("clear", "CLEARED"))

    def list(self) -> list[tuple[str, str]]:
        def attempt() -> list[tuple[str, str]]:
            items: list[str] = []
            count = int(self._expect_collect("list", "END", items))
            chunks: dict[int, str] = {}
            for item in items:
                index, _, data = item.partition(" ")
                chunks[int(index)] = chunks.get(int(index), "") + data.strip()
            if len(chunks) != count:
                raise MeetingError(f"badge reported {count} meetings but sent {len(chunks)}")
            meetings = []
            for index in sorted(chunks):
                try:
                    text = base64.b64decode(chunks[index], validate=True).decode("utf-8")
                    name, url = text.split("\t", 1)
                except ValueError:
                    raise MeetingError(f"meeting {index} arrived garbled") from None
                meetings.append((name, url))
            return meetings

        return self._with_retry("listing", attempt)

    def _expect_collect(self, args: str, prefix: str, collect: list[str]) -> str:
        reply = self._command(args, collect)
        if not reply.startswith(prefix):
            raise MeetingError(f"badge answered {reply!r} to `meet {args}`")
        return reply[len(prefix):].strip()

    def enter(self, setting: Optional[bool] = None) -> bool:
        args = "enter" if setting is None else ("enter on" if setting else "enter off")
        reply = self._expect(args, "ENTER")
        if reply not in ("on", "off"):
            raise MeetingError(f"badge answered {reply!r}")
        return reply == "on"

    def push(self, meetings: list[tuple[str, str]], replace: bool = False, progress=None) -> dict:
        """Add or update `meetings`. With `replace`, also delete anything on the badge not in the list.

        Existing entries are only deleted after every new one has been written, so an interrupted push
        never leaves the badge with fewer working links than it started with.
        """
        before = dict(self.list())
        wanted = dict(meetings)
        result = {"added": 0, "updated": 0, "unchanged": 0, "deleted": 0}
        for name, url in meetings:
            if before.get(name) == url:
                result["unchanged"] += 1
                continue
            self.add(name, url)
            result["updated" if name in before else "added"] += 1
            if progress:
                progress(name)
        if replace:
            for name in before:
                if name not in wanted:
                    self.delete(name)
                    result["deleted"] += 1
        after = dict(self.list())
        missing = [name for name, url in wanted.items() if after.get(name) != url]
        if missing:
            raise MeetingError(f"badge did not keep: {', '.join(missing)}")
        if replace and set(after) != set(wanted):
            raise MeetingError("badge still has entries that are not in the file")
        return result


# -- Serial port ----------------------------------------------------------------------------------

def find_port(explicit: Optional[str]) -> str:
    if explicit:
        return explicit
    if os.environ.get(ENV_PORT_VAR):
        return os.environ[ENV_PORT_VAR]
    try:
        from serial.tools import list_ports
    except ImportError:
        raise MeetingError("pyserial is required: python -m pip install pyserial") from None
    ports = [p for p in list_ports.comports() if (p.vid, p.pid) in KNOWN_USB_IDS]
    if len(ports) == 1:
        return ports[0].device
    seen = "\n".join(f"  {p.device}  {p.description}" for p in list_ports.comports() if p.vid is not None)
    if not ports:
        raise MeetingError("no DC34 badge found. Plug it in, or pass --port.\nSerial devices seen:\n"
                           + (seen or "  (none)"))
    raise MeetingError("more than one badge found; pass --port.\n" + seen)


def open_badge(args) -> Badge:
    port = find_port(args.port)
    try:
        import serial
    except ImportError:
        raise MeetingError("pyserial is required: python -m pip install pyserial") from None
    try:
        link = serial.Serial(port, baudrate=BAUD_RATE, timeout=0.2)
    except serial.SerialException as e:
        hint = ""
        if "ermission" in str(e):
            hint = "\nOn Linux, add yourself to the group that owns the port (usually dialout) and log in again."
        raise MeetingError(f"cannot open {port}: {e}{hint}") from None
    link.reset_input_buffer()
    badge = Badge(link, timeout=args.timeout, line_delay=args.delay, verbose=args.verbose)
    badge.hello()
    return badge


# -- Command line ---------------------------------------------------------------------------------

def read_meetings_file(path: str) -> list[tuple[str, str]]:
    try:
        with open(path, encoding="utf-8") as f:
            return parse_file(f)
    except OSError as e:
        raise MeetingError(f"cannot read {path}: {e.strerror}") from None


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="Manage meeting links on a DC34 badge over USB serial.")
    parser.add_argument("--port", help=f"serial port (default: auto-detect, or ${ENV_PORT_VAR})")
    parser.add_argument("--timeout", type=float, default=15.0, help="seconds to wait for each reply")
    parser.add_argument("--delay", type=float, default=0.0, help="extra seconds to pause between lines")
    parser.add_argument("--verbose", action="store_true", help="show everything the badge sends")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("list", help="show the meetings on the badge")
    sub.add_parser("pull", help="print the badge's meetings in file format")
    p = sub.add_parser("add", help="add or replace one meeting")
    p.add_argument("name")
    p.add_argument("url")
    p = sub.add_parser("del", help="delete one meeting")
    p.add_argument("name")
    p = sub.add_parser("push", help="load a meetings file onto the badge")
    p.add_argument("file")
    p.add_argument("--replace", action="store_true", help="also delete badge entries that are not in the file")
    p = sub.add_parser("clear", help="delete every meeting")
    p.add_argument("--yes", action="store_true", help="do not ask for confirmation")
    p = sub.add_parser("enter", help="whether the badge presses Enter after typing a link")
    p.add_argument("setting", choices=["on", "off", "show"], nargs="?", default="show")
    p = sub.add_parser("check", help="validate a meetings file without touching the badge")
    p.add_argument("file")
    p = sub.add_parser("qr", help="print the text to encode in a QR code for one meeting")
    p.add_argument("name")
    p.add_argument("url")
    args = parser.parse_args(argv)

    try:
        if args.command == "check":
            meetings = read_meetings_file(args.file)
            print(f"{args.file}: {len(meetings)} meetings, all valid")
            return 0
        if args.command == "qr":
            print(qr_payload(args.name, args.url))
            return 0
        if args.command == "push":
            # validate the whole file before opening the port
            meetings = read_meetings_file(args.file)
        if args.command == "add":
            validate(args.name, args.url)

        badge = open_badge(args)
        if args.command == "list":
            meetings = badge.list()
            for name, url in meetings:
                print(f"{name}\n    {url}")
            print(f"{len(meetings)} meetings; Enter after typing: {'on' if badge.enter() else 'off'}")
        elif args.command == "pull":
            for name, url in badge.list():
                print(f"{name} | {url}")
        elif args.command == "add":
            badge.add(args.name, args.url)
            print(f"saved {args.name.strip()!r}")
        elif args.command == "del":
            print("deleted" if badge.delete(args.name) else f"no meeting named {args.name.strip()!r}")
        elif args.command == "push":
            result = badge.push(meetings, replace=args.replace, progress=lambda n: print(f"  sent {n}"))
            print(", ".join(f"{count} {what}" for what, count in result.items()))
        elif args.command == "clear":
            if not args.yes and input("Delete every meeting on the badge? [y/N] ").strip().lower() != "y":
                print("nothing deleted")
                return 0
            print(f"deleted {badge.clear()} meetings")
        elif args.command == "enter":
            setting = None if args.setting == "show" else args.setting == "on"
            print(f"Enter after typing: {'on' if badge.enter(setting) else 'off'}")
        return 0
    except MeetingError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
