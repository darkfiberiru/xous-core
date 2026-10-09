# DC34 badge: meeting links typed over USB

This fork adds a **Meetings** list to the DC34 badge's token mode. Each entry is a name and a URL.
Pick one, press left, and the badge types the URL into whatever computer it is plugged into over
USB-C, as a keyboard, followed by Enter.

The existing Password and TOTP lists and the FIDO2 token are unchanged. The lists now cycle
Password -> TOTP -> Meetings -> Password with the right button.

> [!WARNING]
> **Flashing this erases the badge's vault and weakens its protection. Read this first.**
>
> This is developer-signed firmware. The first time a DC34 badge boots developer-signed firmware,
> the bootloader erases the chip's root secrets. That destroys the light-exchange key *and* makes
> the existing encrypted database (passwords, TOTP secrets, FIDO credentials) unreadable; the badge
> then reformats it without asking. There is no undo.
>
> Afterwards the storage key is derived from erased, known values, and anyone holding the badge
> can load their own firmware with the public developer key. Stored passwords and TOTP secrets
> should be treated as readable, and FIDO credentials as clonable, by anyone with physical access.
>
> Before flashing: add another sign-in method everywhere the badge is registered as a security
> key, make sure you can re-enroll your TOTP accounts, and copy out any passwords you need.
> Do not keep credentials you care about on a developer-mode badge.

## Using it

| Button | In the Meetings list |
|---|---|
| Up / down | Move through the list (hold to page) |
| Left | Type the selected link into the computer |
| Right | Go to the Password list |
| Middle | Scan a QR code |
| Menu | "Delete item", "Meeting Enter key...", "Set keymap...", "USB serial..." |

Put the cursor where the link should go first (a browser's address bar, or the Run box on
Windows), then press left. The badge shows "Typing link..." until it is done.

- **Speed.** Typing runs at the same rate as password autotype, roughly 16 characters a second. A
  short link takes a few seconds; a 400-character calendar link takes about 25 seconds. The badge
  does nothing else while it types, including answering FIDO requests.
- **Enter.** On by default. Turn it off under "Meeting Enter key..." or with
  `dc34_meetings.py enter off`.
- **Keyboard layout.** The badge sends key positions, so its keymap must match the computer's
  layout. This fork defaults a freshly formatted badge to QWERTY (upstream defaults to Dvorak).
  If links come out scrambled, change it under "Set keymap...". Only QWERTY and Dvorak are offered.
- **Editing.** There is no on-badge editor. Add a meeting again with the same name to replace it.

## Adding links by QR code

Open `meeting-qr.html` in a browser. It works offline. Enter a name and link (or paste a list), then
on the badge press the middle button in any token-mode list and point the camera at the code. The
badge shows the name and the whole link, a part at a time if it is long, and saves only if you
choose Yes. Read it: a QR code can come from anyone, and what it stores will be typed into your
computer.

A link in a QR code must start with `https://` or `http://`. Other kinds, such as `zoommtg://`,
can only be loaded over USB.

The QR text is `meeting://<name>|<url>`; `dc34_meetings.py qr NAME URL` prints it if you want to
make the code with something else.

Long links make dense codes that the badge's camera may not read. Use USB for those.

## Adding links over USB

Requires Python 3.9+ and `pyserial` (`python -m pip install pyserial`).

On the badge, open the menu and choose **USB serial...** then **Yes**. This has to be done after
each power-up. Then:

```sh
python dc34_meetings.py push meetings.txt             # add or update everything in the file
python dc34_meetings.py push meetings.txt --replace   # also remove entries not in the file
python dc34_meetings.py list
python dc34_meetings.py pull > meetings.txt           # save what is on the badge
python dc34_meetings.py add "Weekly sync" https://example.com/j/123
python dc34_meetings.py del "Weekly sync"
python dc34_meetings.py check meetings.txt            # validate a file without the badge
```

A meetings file has one `Name | URL` per line; see `meetings.example.txt`. The port is found
automatically; pass `--port` (for example `/dev/ttyACM0`, `COM3`) or set `DC34_MEETINGS_PORT` if
it is not. On Linux you may need to be in the `dialout` group.

While USB serial is enabled, the connected computer can add and remove meetings without any
confirmation on the badge. Leave it off (the default after power-up) when plugging into computers
you do not control.

## Rules for names and links

- Name: up to 48 bytes, no `:` or `|`, no control characters. Names are unique.
- Link: up to 1024 characters of printable ASCII with no spaces. Write a space as `%20`.

These are enforced on the badge when a link is added and again every time one is read back, so
a damaged or hostile entry cannot make the badge type anything other than those characters plus
the optional Enter. Over USB any scheme is allowed, so app links such as `zoommtg://` work.

The badge cannot know what a piece of text will do when typed. A link typed into a terminal
instead of a browser is a command line, so check where the cursor is before pressing left.

## Building and flashing

From the repository root, with the Xous toolkit installed (`cargo xtask install-toolkit`):

```sh
cargo xtask baosec-lite dc34-console~flash dc34-vault --no-verify
```

This produces `loader.uf2`, `xous.uf2` and `swap.uf2` in
`target/riscv32imac-unknown-xous-elf/release/`. The fork's "DC34 Meetings firmware" GitHub Actions
workflow builds the same three files and attaches them to each run.

To flash, hold any button while plugging the module in so it appears as a USB drive, copy the
three `.uf2` files onto it (on Linux, run `sync`), then press any button. Full steps are at
<https://defcon.org/34b/>.

## Tests

```sh
rustc --edition 2021 --test ../../libs/dc34-api/src/meeting.rs -o /tmp/meeting-tests && /tmp/meeting-tests
cargo build --release --manifest-path harness/Cargo.toml
python test_dc34_meetings.py
```

`harness/` compiles the firmware's real protocol, validation and storage code for the host on top
of an in-memory stand-in for the badge's database, and the Python tests drive it through
`dc34_meetings.py`. The USB transport, the display code and the typing itself only run on a badge.

## Where the code is

| File | What it does |
|---|---|
| `libs/dc34-api/src/meeting.rs` | Record format, validation, QR and serial parsing, unit tests |
| `libs/dc34-api/src/meeting_store.rs` | Reading and writing records in the badge's database |
| `apps-baosec/dc34-vault/src/meetcmd.rs` | The `meet` serial protocol |
| `apps-baosec/dc34-vault/src/ux.rs` | Meetings screen, navigation, typing |
| `apps-baosec/dc34-vault/src/actions.rs` | QR import, list loading, delete |
| `apps-baosec/dc34-vault/src/main.rs` | Mode, Enter setting, QWERTY default |
| `services/dc34-console/src/cmds/meet.rs` | Forwards `meet` lines from the console to the vault |
