/**
 * What Live Editor keeps in this browser, in IndexedDB: the apps (sources, versions, chats and the
 * preview's storage), the settings, and the signing key. Nothing leaves the browser but the
 * requests to the model service and the packages sent to a device.
 */

const NAME = "barracuda-live-editor";
const VERSION = 1;
const STORES = ["projects", "settings", "keys", "transcripts"] as const;
export type StoreName = (typeof STORES)[number];

let opened: Promise<IDBDatabase> | undefined;

function request<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

function database(): Promise<IDBDatabase> {
  opened ??= new Promise((resolve, reject) => {
    const req = indexedDB.open(NAME, VERSION);
    req.onupgradeneeded = () => {
      for (const store of STORES)
        if (!req.result.objectStoreNames.contains(store))
          req.result.createObjectStore(store);
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
  return opened;
}

async function store(name: StoreName, mode: IDBTransactionMode) {
  return (await database()).transaction(name, mode).objectStore(name);
}

/** Reads one record. */
export async function get<T>(
  name: StoreName,
  key: string,
): Promise<T | undefined> {
  return (await request((await store(name, "readonly")).get(key))) as
    T | undefined;
}

/** Reads every record of a store. */
export async function all<T>(name: StoreName): Promise<T[]> {
  return (await request((await store(name, "readonly")).getAll())) as T[];
}

/** Writes one record. */
export async function put(
  name: StoreName,
  key: string,
  value: unknown,
): Promise<void> {
  await request((await store(name, "readwrite")).put(value, key));
}

/** Deletes one record. */
export async function remove(name: StoreName, key: string): Promise<void> {
  await request((await store(name, "readwrite")).delete(key));
}
