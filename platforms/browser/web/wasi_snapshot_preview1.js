// Minimal WASI Preview 1 host used by Lua's statically linked C runtime.

const ERRNO_SUCCESS = 0;
const ERRNO_BADF = 8;
const ERRNO_INVAL = 28;
const ERRNO_NOENT = 44;
const ERRNO_SPIPE = 70;
const FILETYPE_CHARACTER_DEVICE = 2;

let memory;

export function setMemory(value) {
  memory = value;
}

function view() {
  if (!memory) throw new Error("WASI memory is not initialized");
  return new DataView(memory.buffer);
}

export function environ_get() {
  return ERRNO_SUCCESS;
}

export function environ_sizes_get(count, size) {
  const data = view();
  data.setUint32(count, 0, true);
  data.setUint32(size, 0, true);
  return ERRNO_SUCCESS;
}

export function clock_time_get(clock, _precision, result) {
  if (clock !== 0 && clock !== 1) return ERRNO_INVAL;
  const millis = clock === 1 ? performance.now() : Date.now();
  view().setBigUint64(result, BigInt(Math.trunc(millis * 1_000_000)), true);
  return ERRNO_SUCCESS;
}

export function fd_close(fd) {
  return fd <= 2 ? ERRNO_SUCCESS : ERRNO_BADF;
}

export function fd_fdstat_get(fd, result) {
  if (fd > 2) return ERRNO_BADF;
  const data = view();
  for (let offset = 0; offset < 24; offset += 1)
    data.setUint8(result + offset, 0);
  data.setUint8(result, FILETYPE_CHARACTER_DEVICE);
  const rights = fd === 0 ? 1n << 1n : 1n << 6n;
  data.setBigUint64(result + 8, rights, true);
  data.setBigUint64(result + 16, rights, true);
  return ERRNO_SUCCESS;
}

export function fd_fdstat_set_flags(fd) {
  return fd <= 2 ? ERRNO_SUCCESS : ERRNO_BADF;
}

export function fd_prestat_get() {
  return ERRNO_BADF;
}

export function fd_prestat_dir_name() {
  return ERRNO_BADF;
}

export function fd_read(fd, _iovs, _iovsLength, bytesRead) {
  if (fd !== 0) return ERRNO_BADF;
  view().setUint32(bytesRead, 0, true);
  return ERRNO_SUCCESS;
}

export function fd_renumber() {
  return ERRNO_BADF;
}

export function fd_seek(fd, _offset, _whence, newOffset) {
  if (fd > 2) return ERRNO_BADF;
  view().setBigUint64(newOffset, 0n, true);
  return ERRNO_SPIPE;
}

export function fd_write(fd, iovs, iovsLength, bytesWritten) {
  if (fd !== 1 && fd !== 2) return ERRNO_BADF;
  const data = view();
  const decoder = new TextDecoder();
  const chunks = [];
  let total = 0;
  for (let index = 0; index < iovsLength; index += 1) {
    const entry = iovs + index * 8;
    const pointer = data.getUint32(entry, true);
    const length = data.getUint32(entry + 4, true);
    chunks.push(decoder.decode(new Uint8Array(memory.buffer, pointer, length)));
    total += length;
  }
  (fd === 2 ? console.error : console.log)(chunks.join("").replace(/\n$/, ""));
  data.setUint32(bytesWritten, total, true);
  return ERRNO_SUCCESS;
}

export function path_open() {
  return ERRNO_NOENT;
}

export function proc_exit(code) {
  throw new Error(`Barracuda exited with status ${code}`);
}
