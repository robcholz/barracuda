"""Local SNTP responder so time-dependent scenarios never need public NTP.

The System resolves its fixed public NTP pool and sends UDP/123 through the
host TUN. A DNAT rule redirects that traffic to this responder on the TUN
peer address, which answers with the host clock.
"""

from __future__ import annotations

import socket
import struct
import subprocess
import threading
import time

from .system import HOST_ADDRESS, INTERFACE, HarnessError

NTP_PORT = 123
NTP_UNIX_OFFSET = 2_208_988_800
DNAT_RULE = [
    'PREROUTING',
    '-i',
    INTERFACE,
    '-p',
    'udp',
    '--dport',
    str(NTP_PORT),
    '-j',
    'DNAT',
    '--to-destination',
    f'{HOST_ADDRESS}:{NTP_PORT}',
]


def ntp_timestamp(unix_seconds: float) -> bytes:
    """Encode a Unix time as a 64-bit NTP timestamp."""

    seconds = int(unix_seconds) + NTP_UNIX_OFFSET
    fraction = int((unix_seconds % 1) * (1 << 32))
    return struct.pack('>II', seconds & 0xFFFF_FFFF, fraction & 0xFFFF_FFFF)


def sntp_reply(request: bytes, now: float) -> bytes:
    """Build an SNTPv4 server response to a client request."""

    if len(request) < 48:
        raise ValueError('SNTP request is shorter than 48 bytes')
    version = (request[0] >> 3) & 0b111 or 4
    header = struct.pack(
        '>BBbbII4s',
        (version << 3) | 4,  # leap indicator 0, server mode
        1,  # primary reference
        6,  # poll interval
        -20,  # precision
        0,  # root delay
        0,  # root dispersion
        b'LOCL',
    )
    stamp = ntp_timestamp(now)
    return header + stamp + request[40:48] + stamp + stamp


class LocalNtp:
    """Answers SNTP for the System while a scenario runs."""

    def __init__(self) -> None:
        self._socket: socket.socket | None = None
        self._thread: threading.Thread | None = None
        self._stopping = threading.Event()

    def __enter__(self) -> LocalNtp:
        self._socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._socket.settimeout(0.2)
        try:
            self._socket.bind((HOST_ADDRESS, NTP_PORT))
        except OSError as exc:
            raise HarnessError(f'cannot bind local NTP responder: {exc}') from exc
        _iptables('-A')
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()
        return self

    def __exit__(self, *_exc: object) -> None:
        self._stopping.set()
        if self._thread is not None:
            self._thread.join()
        if self._socket is not None:
            self._socket.close()
        _iptables('-D')

    def _serve(self) -> None:
        assert self._socket is not None
        while not self._stopping.is_set():
            try:
                request, peer = self._socket.recvfrom(512)
            except TimeoutError:
                continue
            try:
                self._socket.sendto(sntp_reply(request, time.time()), peer)
            except ValueError:
                continue


def _iptables(action: str) -> None:
    result = subprocess.run(
        ['iptables', '-t', 'nat', action, *DNAT_RULE],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0 and action == '-A':
        raise HarnessError(f'cannot install NTP redirect: {result.stderr.strip()}')
