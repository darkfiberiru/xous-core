#!/usr/bin/env python3
"""
End-to-end tests for dc34_meetings.py.

The "badge" on the other end is harness/ - a host build of the real firmware protocol code
(meetcmd.rs, meeting.rs, meeting_store.rs) on top of an in-memory stand-in for the PDDB. So these
tests exercise the same parsing, validation and storage logic that runs on the device; only the
USB transport and the display are not covered.

    cargo build --release --manifest-path harness/Cargo.toml
    python3 test_dc34_meetings.py
"""

import os
import subprocess
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import dc34_meetings as dm  # noqa: E402

HARNESS = os.path.join(HERE, "harness", "target", "release", "dc34-meetings-harness")

LONG_URL = "https://teams.example.com/l/meetup-join/19%3ameeting_" + "Ab9_" * 240 + "?context=%7b%22Tid%22%7d"


class Pipe:
    """Serial-port lookalike over the harness's stdin/stdout, with optional line corruption."""

    def __init__(self):
        self.proc = subprocess.Popen([HARNESS], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        self.corrupt_next_data = 0
        self.sent: list[str] = []

    def write(self, data: bytes):
        line = data.decode()
        if self.corrupt_next_data and line.startswith("meet data "):
            # simulate a dropped character on the wire
            self.corrupt_next_data -= 1
            data = (line[:12] + line[13:]).encode()
        self.sent.append(line)
        self.proc.stdin.write(data)

    def flush(self):
        self.proc.stdin.flush()

    def readline(self) -> bytes:
        return self.proc.stdout.readline()

    def raw(self, line: str) -> None:
        """Send a non-`meet` line and swallow the three lines the harness answers with."""
        self.proc.stdin.write((line + "\n").encode())
        self.proc.stdin.flush()
        for _ in range(3):
            self.proc.stdout.readline()

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=5)
        self.proc.stdout.close()


class StockFirmware:
    """A badge whose console has never heard of `meet`."""

    def __init__(self):
        self.pending: list[bytes] = []

    def write(self, data: bytes):
        self.pending = [b"[console] " + data, b"Commands: echo, ver, test, image, bio\r\n"]

    def readline(self) -> bytes:
        return self.pending.pop(0) if self.pending else b""


class Silent:
    """A badge with USB serial input not enabled: nothing ever comes back."""

    def write(self, data: bytes):
        pass

    def readline(self) -> bytes:
        return b""


class DeviceTests(unittest.TestCase):
    def setUp(self):
        self.pipe = Pipe()
        self.badge = dm.Badge(self.pipe, timeout=5)
        self.badge.hello()

    def tearDown(self):
        self.pipe.close()

    def test_starts_empty_with_enter_on(self):
        self.assertEqual(self.badge.list(), [])
        self.assertTrue(self.badge.enter())

    def test_add_list_roundtrip(self):
        self.badge.add("Weekly sync", "https://example.zoom.us/j/1234567890?pwd=aBc:dEf%20_-~")
        self.badge.add("  Standup  ", "https://meet.example.com/abc-defg-hij")
        self.assertEqual(self.badge.list(), [
            ("Standup", "https://meet.example.com/abc-defg-hij"),
            ("Weekly sync", "https://example.zoom.us/j/1234567890?pwd=aBc:dEf%20_-~"),
        ])

    def test_longest_url_survives(self):
        url = LONG_URL[:dm.URL_MAX]
        self.assertEqual(len(url), dm.URL_MAX)
        name = "n" * dm.NAME_MAX
        self.badge.add(name, url)
        self.assertEqual(self.badge.list(), [(name, url)])

    def test_many_long_entries_exceed_bulk_read(self):
        # 80 x ~1 kB is past the firmware's 64 kB bulk-read budget, forcing the one-at-a-time path
        url = LONG_URL[:1000]
        for i in range(80):
            self.badge.add(f"m{i:02d}", url)
        listed = self.badge.list()
        self.assertEqual(len(listed), 80)
        self.assertTrue(all(u == url for _, u in listed))

    def test_replacing_with_shorter_url_leaves_no_tail(self):
        self.badge.add("x", "https://example.com/" + "long" * 50)
        self.badge.add("x", "https://example.com/s")
        self.assertEqual(self.badge.list(), [("x", "https://example.com/s")])

    def test_unicode_link_over_serial(self):
        self.badge.add("Café", "https://bücher.example/café?q=日本")
        self.assertEqual(self.badge.list(),
                         [("Café", "https://xn--bcher-kva.example/caf%C3%A9?q=%E6%97%A5%E6%9C%AC")])
        # the badge itself still refuses raw non-ASCII, so nothing untypeable can be stored
        reply = self.badge._command("save " + self.badge._stage("x\thttps://bücher.example/".encode()))
        self.assertTrue(reply.startswith("ERR"), reply)

    def test_non_ascii_name(self):
        self.badge.add("Réunion d'équipe ☕", "https://example.com/r")
        self.assertEqual(self.badge.list(), [("Réunion d'équipe ☕", "https://example.com/r")])

    def test_delete(self):
        self.badge.add("a", "https://example.com/a")
        self.assertTrue(self.badge.delete("a"))
        self.assertFalse(self.badge.delete("a"))
        self.assertEqual(self.badge.list(), [])

    def test_clear_many_max_length_names(self):
        # more names than fit in one bulk-delete message on the device
        for i in range(90):
            self.badge.add(f"{i:02d}" + "n" * 46, "https://example.com/x")
        self.assertEqual(self.badge.clear(), 90)
        self.assertEqual(self.badge.list(), [])

    def test_clear(self):
        self.assertEqual(self.badge.clear(), 0)
        for n in "abc":
            self.badge.add(n, "https://example.com/" + n)
        self.assertEqual(self.badge.clear(), 3)
        self.assertEqual(self.badge.list(), [])

    def test_enter_setting(self):
        self.assertFalse(self.badge.enter(False))
        self.assertFalse(self.badge.enter())
        self.assertTrue(self.badge.enter(True))

    def test_dropped_character_is_retried(self):
        self.pipe.corrupt_next_data = 1
        self.badge.add("flaky", "https://example.com/flaky")
        self.assertEqual(self.badge.list(), [("flaky", "https://example.com/flaky")])
        self.assertEqual(sum(1 for s in self.pipe.sent if s.startswith("meet begin")), 2)

    def test_persistent_corruption_fails_and_saves_nothing(self):
        self.pipe.corrupt_next_data = 99
        with self.assertRaises(dm.MeetingError):
            self.badge.add("never", "https://example.com/never")
        self.pipe.corrupt_next_data = 0
        self.assertEqual(self.badge.list(), [])

    def test_firmware_rejects_what_the_host_would(self):
        # bypass host-side validation to prove the device enforces the rules by itself
        def raw_save(payload: bytes) -> str:
            return self.badge._command("save " + self.badge._stage(payload))

        for payload in [
            b"name\thttps://example.com/has space",
            b"name\thttps://example.com/\ncalc.exe",
            "name\thttps://example.com/é".encode(),
            b"bad:name\thttps://example.com/",
            b"bad|name\thttps://example.com/",
            b"\thttps://example.com/",
            b"name\t",
            b"no separator at all",
            b"n" * 49 + b"\thttps://example.com/",
            b"name\t" + b"u" * 1025,
        ]:
            reply = raw_save(payload)
            self.assertTrue(reply.startswith("ERR"), (payload[:30], reply))
        self.assertEqual(self.badge.list(), [])

    def test_wrong_crc_and_stale_commit(self):
        self.badge._stage(b"a\thttps://example.com/a")
        self.assertTrue(self.badge._command("save 00000000").startswith("ERR"))
        # the failed commit emptied the buffer, so even the right CRC can't complete it now
        crc = f"{dm.zlib.crc32(b'a' + bytes([9]) + b'https://example.com/a'):08x}"
        self.assertTrue(self.badge._command("save " + crc).startswith("ERR"))
        self.assertEqual(self.badge.list(), [])

    def test_oversized_and_garbage_chunks(self):
        big = dm.base64.b64encode(b"x" * 49).decode()
        self.assertTrue(self.badge._command("data " + big).startswith("ERR"))
        self.assertTrue(self.badge._command("data !!!not-base64!!!").startswith("ERR"))
        self.assertTrue(self.badge._command("frobnicate").startswith("ERR"))

    def test_corrupt_record_is_skipped_not_fatal(self):
        self.badge.add("good", "https://example.com/good")
        self.pipe.raw("corrupt broken")
        self.assertEqual(self.badge.list(), [("good", "https://example.com/good")])

    def test_push_merge_and_replace(self):
        self.badge.add("old", "https://example.com/old")
        self.badge.add("keep", "https://example.com/keep")
        wanted = [("keep", "https://example.com/keep"), ("new", "https://example.com/new"),
                  ("old", "https://example.com/changed")]
        self.assertEqual(self.badge.push(wanted),
                         {"added": 1, "updated": 1, "unchanged": 1, "deleted": 0})
        self.badge.add("extra", "https://example.com/extra")
        self.assertEqual(self.badge.push(wanted[:2], replace=True),
                         {"added": 0, "updated": 0, "unchanged": 2, "deleted": 2})
        self.assertEqual(self.badge.list(), wanted[:2])


class HostTests(unittest.TestCase):
    def test_stock_firmware_is_reported_clearly(self):
        with self.assertRaisesRegex(dm.MeetingError, "no `meet` command"):
            dm.Badge(StockFirmware(), timeout=1).hello()

    def test_silent_badge_points_at_usb_serial_menu(self):
        with self.assertRaisesRegex(dm.MeetingError, "USB serial"):
            dm.Badge(Silent(), timeout=0.3).hello()

    def test_parse_file(self):
        text = """
        # work
        Weekly sync | https://example.com/j/1?a=b|c
        Standup|https://example.com/s
        """
        self.assertEqual(dm.parse_file(text.splitlines()), [
            ("Weekly sync", "https://example.com/j/1?a=b|c"),
            ("Standup", "https://example.com/s"),
        ])

    def test_parse_file_errors_name_the_line(self):
        for text, pattern in [
            ("just a url https://example.com", "line 1: expected"),
            ("ok | https://example.com/a\nok | https://example.com/b", "line 2: 'ok' appears twice"),
            ("a | https://example.com/has space", "line 1: .*space"),
            ("a:b | https://example.com/", "line 1: .*':'"),
            (" | https://example.com/", "line 1: name is empty"),
        ]:
            with self.assertRaisesRegex(dm.MeetingError, pattern):
                dm.parse_file(text.splitlines())

    def test_unicode_links_become_ascii(self):
        # expected values are what Chromium's URL parser produces for the same input
        cases = {
            "https://bücher.example/café?q=日本#frag-é":
                "https://xn--bcher-kva.example/caf%C3%A9?q=%E6%97%A5%E6%9C%AC#frag-%C3%A9",
            "https://例え.テスト/パス": "https://xn--r8jz45g.xn--zckzah/%E3%83%91%E3%82%B9",
            "https://user:pä@münchen.example:8443/a": "https://user:p%C3%A4@xn--mnchen-3ya.example:8443/a",
            "zoommtg://zoom.us/join?name=José": "zoommtg://zoom.us/join?name=Jos%C3%A9",
            # ASCII input is never rewritten, even where a browser would normalise it
            "https://EXAMPLE.com/a%20b?x=1&y=2": "https://EXAMPLE.com/a%20b?x=1&y=2",
        }
        for given, expected in cases.items():
            self.assertEqual(dm.to_ascii_url(given), expected)
            self.assertEqual(dm.validate("n", given), ("n", expected))
        # characters that IDNA 2003 and browsers map differently are refused rather than guessed
        with self.assertRaisesRegex(dm.MeetingError, "xn--"):
            dm.to_ascii_url("https://straße.example/")
        # a space is still an error, not something to paper over
        with self.assertRaisesRegex(dm.MeetingError, "space"):
            dm.validate("n", "https://example.com/caf é")

    def test_qr_payload(self):
        self.assertEqual(dm.qr_payload(" Standup ", "https://example.com/s"),
                         "meeting://Standup|https://example.com/s")

    def test_cli_check_and_qr(self):
        path = os.path.join(HERE, "meetings.example.txt")
        self.assertEqual(dm.main(["check", path]), 0)
        self.assertEqual(dm.main(["qr", "a", "https://example.com/a"]), 0)
        self.assertEqual(dm.main(["qr", "a", "not typeable"]), 1)
        self.assertEqual(dm.main(["qr", "a", "zoommtg://zoom.us/join?confno=1"]), 1)


if __name__ == "__main__":
    if not os.path.exists(HARNESS):
        sys.exit(f"build the harness first: cargo build --release --manifest-path {HERE}/harness/Cargo.toml")
    unittest.main(verbosity=2)
