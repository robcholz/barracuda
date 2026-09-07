const ROOT = "barracuda";

export async function openPersistentFile(name) {
  const root = await navigator.storage.getDirectory();
  const directory = await root.getDirectoryHandle(ROOT, { create: true });
  return directory.getFileHandle(name, { create: true });
}

export async function writePersistentFile(name, bytes) {
  const handle = await openPersistentFile(name);
  const writable = await handle.createWritable();
  await writable.write(bytes);
  await writable.close();
}

export async function readPersistentFile(name) {
  const handle = await openPersistentFile(name);
  return new Uint8Array(await (await handle.getFile()).arrayBuffer());
}
