"""Build and run an isolated host Barracuda System and its llm-tape peer."""

from __future__ import annotations

import json
import os
import re
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path

from . import virtual_io

WORKSPACE = Path(__file__).resolve().parents[4]
BOARD = 'local-linux'
INTERFACE = 'barracuda0'
STACK_ADDRESS = '10.42.0.2'
HOST_ADDRESS = '10.42.0.1'
WEB_PORT = 8787
READY_PATTERN = r'WebServer listening at'
ERASED = 0xFF


class HarnessError(RuntimeError):
    """The host environment or the System under test is not usable."""


@dataclass(frozen=True)
class Binaries:
    """Host artifacts required by every scenario."""

    system: Path
    cli: Path
    resources_image: Path
    flash_capacity: int
    resources_offset: int


def lean_cargo_env() -> dict[str, str]:
    """Cargo environment matching CI: no incremental state or debuginfo."""

    return {**os.environ, 'CARGO_INCREMENTAL': '0', 'CARGO_PROFILE_DEV_DEBUG': '0'}


def build(skip: bool = False, coverage: Path | None = None) -> Binaries:
    """Build the selected host System, CLI, and Plugin resources image.

    With `coverage`, the System is also built instrumented into that target
    directory and that build is the one returned.
    """

    selected = WORKSPACE / '.barracuda' / 'selected-board'
    if not selected.exists() or selected.read_text().strip() != BOARD:
        raise HarnessError(f'select the host Board first: cargo board select {BOARD}')
    if not skip:
        for command in (
            ['cargo', 'build'],
            ['cargo', 'build', '--package', 'barracuda-cli'],
            ['cargo', 'image', 'build'],
        ):
            _run(command)
    host = host_triple()
    debug = WORKSPACE / 'target' / host / 'debug'
    system = debug / 'barracuda-system'
    if coverage is not None:
        from .coverage import RUSTFLAGS

        if not skip:
            _run(
                ['cargo', 'build'],
                {'CARGO_TARGET_DIR': str(coverage), 'RUSTFLAGS': RUSTFLAGS},
            )
        system = coverage / host / 'debug' / 'barracuda-system'
    capacity, offset = _resources_region()
    binaries = Binaries(
        system=system,
        cli=debug / 'barracuda',
        resources_image=WORKSPACE / 'target' / 'barracuda-system.img',
        flash_capacity=capacity,
        resources_offset=offset,
    )
    for path in (binaries.system, binaries.cli, binaries.resources_image):
        if not path.exists():
            raise HarnessError(f'missing build artifact {path}')
    return binaries


def require_network() -> None:
    """Fail early when the provisioned host TUN is absent."""

    if not Path('/sys/class/net', INTERFACE).exists():
        raise HarnessError(
            f'TUN {INTERFACE} is not provisioned; run: '
            f'sudo sh platforms/linux/provision.sh {INTERFACE}'
        )


class SystemProcess:
    """One System run against a freshly erased flash image."""

    def __init__(
        self,
        binaries: Binaries,
        state_dir: Path,
        log_path: Path,
        heap_limit: int | None = None,
    ):
        self._binaries = binaries
        self._state_dir = state_dir
        self._log_path = log_path
        self._heap_limit = heap_limit
        self._process: subprocess.Popen[bytes] | None = None
        # Exit status when the System stopped on its own before `stop`.
        self.early_exit: int | None = None

    def start(self, timeout: float = 60.0) -> None:
        """Write a fresh flash image, launch the System, and wait for HTTP."""

        flash = self._state_dir / '.barracuda' / 'board.flash'
        flash.parent.mkdir(parents=True, exist_ok=True)
        image = self._binaries.resources_image.read_bytes()
        contents = bytearray([ERASED]) * self._binaries.flash_capacity
        offset = self._binaries.resources_offset
        contents[offset : offset + len(image)] = image
        flash.write_bytes(contents)
        log = self._log_path.open('wb')
        environment = dict(os.environ)
        environment[virtual_io.ADDRESS_VARIABLE] = virtual_io.address_value()
        if self._heap_limit is not None:
            environment['BARRACUDA_HEAP_LIMIT_BYTES'] = str(self._heap_limit)
        self._process = subprocess.Popen(
            [str(self._binaries.system)],
            cwd=self._state_dir,
            env=environment,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        self.wait_for_log(READY_PATTERN, timeout)

    def wait_for_log(self, pattern: str, timeout: float) -> None:
        """Wait until `pattern` appears in the System log."""

        deadline = time.monotonic() + timeout
        regex = re.compile(pattern)
        while time.monotonic() < deadline:
            if regex.search(self.log()):
                return
            if self._process is not None and self._process.poll() is not None:
                raise HarnessError(f'System exited with {self._process.returncode}')
            time.sleep(0.2)
        raise HarnessError(f'timed out waiting for System log {pattern!r}')

    def log(self) -> str:
        """Current System log contents."""

        return self._log_path.read_text(encoding='utf-8', errors='replace')

    def stop(self) -> None:
        """Interrupt the System and wait for it to exit."""

        if self._process is None:
            return
        if self._process.poll() is not None:
            self.early_exit = self._process.returncode
            return
        os.killpg(self._process.pid, signal.SIGINT)
        try:
            self._process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(self._process.pid, signal.SIGKILL)
            self._process.wait()


class TapeServer:
    """An llm-tape record or replay server reachable from the System."""

    def __init__(self, port: int, log_path: Path):
        self.port = port
        self._log_path = log_path
        self._process: subprocess.Popen[bytes] | None = None

    def replay(self, tape: Path, capture_requests: Path | None = None) -> None:
        """Serve `tape` in recorded order, optionally keeping request bodies."""

        arguments = ['replay', '--listen', f'0.0.0.0:{self.port}']
        if capture_requests is not None:
            arguments += ['--capture-requests', str(capture_requests)]
        self._start([*arguments, str(tape)])

    def record(self, upstream: str, tape: Path) -> None:
        """Proxy to `upstream` and append every interaction to `tape`."""

        tape.parent.mkdir(parents=True, exist_ok=True)
        self._start(
            [
                'record',
                '--listen',
                f'0.0.0.0:{self.port}',
                '--upstream',
                upstream,
                '--output',
                str(tape),
            ]
        )

    def base_url(self, api_path: str) -> str:
        """Model API base URL as seen from the System's IP stack."""

        return f'http://{HOST_ADDRESS}:{self.port}{api_path}'

    def counts(self) -> dict[str, int]:
        """Replay outcome counters parsed from the server log."""

        log = self._log_path.read_text(encoding='utf-8', errors='replace')
        return {
            name: len(re.findall(rf'replay {name}\b', log))
            for name in ('request_completed', 'request_rejected')
        }

    def stop(self) -> None:
        """Stop the server; a recorder closes its tape on interrupt."""

        if self._process is None or self._process.poll() is not None:
            return
        self._process.send_signal(signal.SIGINT)
        try:
            self._process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self._process.kill()
            self._process.wait()

    def _start(self, arguments: Sequence[str]) -> None:
        log = self._log_path.open('wb')
        self._process = subprocess.Popen(
            [sys.executable, '-m', 'llm_tape', *arguments],
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        health = f'http://127.0.0.1:{self.port}/_llm_tape/health'
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self._process.poll() is not None:
                raise HarnessError('llm-tape exited during startup')
            try:
                with _no_proxy_open(health, timeout=1):
                    return
            except OSError:
                time.sleep(0.2)
        raise HarnessError('llm-tape did not become healthy')


def configure_model(base_url: str, model: str, api_key: str) -> None:
    """Install one default OpenAI-compatible model API on the Agent."""

    body = json.dumps(
        [
            {
                'timeout_ms': 60_000,
                'max_tokens': 2_048,
                'image_max_bytes': 524_288,
                'backend': 'openai_compatible',
                'purpose': 'root_agent',
                'default': True,
                'api_key': api_key,
                'model': model,
                'base_url': base_url,
            }
        ]
    ).encode('utf-8')
    request = urllib.request.Request(
        f'http://{STACK_ADDRESS}:{WEB_PORT}/api/model-api',
        data=body,
        method='POST',
        headers={'content-type': 'application/json'},
    )
    with _no_proxy_open(request, timeout=10) as response:
        if response.status != 204:
            raise HarnessError(f'model API configuration returned {response.status}')


def http_request(method: str, path: str, body: str | None) -> tuple[int, str]:
    """Call the System WebServer directly and return status and body."""

    request = urllib.request.Request(
        f'http://{STACK_ADDRESS}:{WEB_PORT}{path}',
        data=None if body is None else body.encode('utf-8'),
        method=method,
        headers={'content-type': 'application/json'} if body is not None else {},
    )
    try:
        with _no_proxy_open(request, timeout=10) as response:
            return response.status, response.read().decode('utf-8', 'replace')
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode('utf-8', 'replace')
    except OSError as error:
        # A hung or refused request fails this check, not the whole run.
        return 0, f'request failed: {error}'


def chat(cli: Path, messages: Sequence[str], timeout: float) -> list[dict[str, object]]:
    """Send `messages` through the scripted CLI Channel and parse its records."""

    completed = subprocess.run(
        [str(cli), 'chat', f'http://{STACK_ADDRESS}:{WEB_PORT}'],
        input='\n'.join(messages) + '\n',
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    if completed.returncode != 0:
        raise HarnessError(f'CLI chat failed: {completed.stderr.strip()}')
    return [json.loads(line) for line in completed.stdout.splitlines() if line.strip()]


def _no_proxy_open(request: str | urllib.request.Request, timeout: float):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    return opener.open(request, timeout=timeout)


def _run(command: Sequence[str], extra_env: dict[str, str] | None = None) -> None:
    result = subprocess.run(
        command,
        cwd=WORKSPACE,
        env={**lean_cargo_env(), **(extra_env or {})},
        check=False,
        capture_output=True,
    )
    if result.returncode != 0:
        output = result.stderr.decode('utf-8', 'replace')[-4000:]
        raise HarnessError(f'{" ".join(command)} failed:\n{output}')


def host_triple() -> str:
    output = subprocess.run(
        ['rustc', '-vV'], capture_output=True, text=True, check=True
    ).stdout
    match = re.search(r'^host: (\S+)$', output, re.MULTILINE)
    if match is None:
        raise HarnessError('cannot determine the Rust host triple')
    return match.group(1)


def _resources_region() -> tuple[int, int]:
    layout = (WORKSPACE / 'boards' / 'configs' / BOARD / 'file-layout.yml').read_text()
    capacity = int(re.search(r'^capacity:\s*(\d+)', layout, re.MULTILINE).group(1))
    for block in re.split(r'\n\s*- ', layout):
        if re.search(r'name:\s*resources\b', block):
            offset = re.search(r'offset:\s*(\d+)', block)
            if offset is not None:
                return capacity, int(offset.group(1))
    raise HarnessError('host file layout has no resources region')
