/**
 * An in-memory file system that stands in for `node:fs` when the MicroTS compiler runs in the
 * browser, and a TypeScript `System` over it. build.ts bundles the compiler with `node:fs`
 * resolved to this module, and loads it with the files the compiler reads: TypeScript's library
 * declarations and PocketJS's own sources.
 */

const files = new Map<string, string>();
const directories = new Set<string>(["/"]);

const parent = (path: string) => path.slice(0, path.lastIndexOf("/")) || "/";

/** Writes `text` at `path`, making its directories. */
export function put(path: string, text: string) {
  files.set(path, text);
  for (
    let directory = parent(path);
    !directories.has(directory);
    directory = parent(directory)
  )
    directories.add(directory);
}

/** Loads many files at once: a path → text record, as build.ts snapshots them. */
export function mount(entries: Record<string, string>) {
  for (const [path, text] of Object.entries(entries)) put(path, text);
}

/** Removes every file below `directory`. */
export function clear(directory: string) {
  const prefix = `${directory}/`;
  for (const path of [...files.keys()])
    if (path.startsWith(prefix)) files.delete(path);
}

const missing = (path: string) =>
  Object.assign(new Error(`ENOENT: no such file or directory, '${path}'`), {
    code: "ENOENT",
  });

function stat(path: string) {
  const text = files.get(path);
  if (text === undefined && !directories.has(path)) throw missing(path);
  return {
    isFile: () => text !== undefined,
    isDirectory: () => text === undefined,
    isSymbolicLink: () => false,
    size: text?.length ?? 0,
    mtimeMs: 0,
    mtime: new Date(0),
  };
}

export const existsSync = (path: string) =>
  files.has(path) || directories.has(path);
export function readFileSync(path: string): string {
  const text = files.get(path);
  if (text === undefined) throw missing(path);
  return text;
}
export const statSync = stat;
export const lstatSync = stat;
export const realpathSync = (path: string) => path;
export const writeFileSync = (path: string, text: string | Uint8Array) =>
  put(path, typeof text === "string" ? text : new TextDecoder().decode(text));
export const mkdirSync = (path: string) => {
  directories.add(path);
};
export const rmSync = (path: string) => clear(path);
export function readdirSync(path: string): string[] {
  const names = new Set<string>();
  for (const entry of [...files.keys(), ...directories])
    if (entry !== path && parent(entry) === path)
      names.add(entry.slice(path.length + 1));
  return [...names];
}
export const promises = {
  readFile: async (path: string) => readFileSync(path),
};

export default {
  existsSync,
  readFileSync,
  statSync,
  lstatSync,
  realpathSync,
  writeFileSync,
  mkdirSync,
  rmSync,
  readdirSync,
  promises,
};

/** The parts of TypeScript the `System` below needs. */
interface TypeScriptLike {
  matchFiles(
    path: string,
    extensions: readonly string[] | undefined,
    exclude: readonly string[] | undefined,
    include: readonly string[] | undefined,
    useCaseSensitiveFileNames: boolean,
    currentDirectory: string,
    depth: number | undefined,
    getEntries: (path: string) => { files: string[]; directories: string[] },
    realpath: (path: string) => string,
  ): string[];
}

/** A TypeScript `System` over these files, with `cwd` as the working directory. */
export function system(ts: TypeScriptLike, cwd: string) {
  const entries = (path: string) => {
    const names = readdirSync(path);
    return {
      files: names.filter((name) => files.has(`${path}/${name}`)),
      directories: names.filter((name) => directories.has(`${path}/${name}`)),
    };
  };
  return {
    args: [],
    newLine: "\n",
    useCaseSensitiveFileNames: true,
    write: () => {},
    writeOutputIsTTY: () => false,
    readFile: (path: string) => files.get(path),
    writeFile: (path: string, text: string) => put(path, text),
    fileExists: (path: string) => files.has(path),
    directoryExists: (path: string) => directories.has(path),
    createDirectory: (path: string) => {
      directories.add(path);
    },
    getExecutingFilePath: () =>
      `${cwd}/node_modules/typescript/lib/typescript.js`,
    getCurrentDirectory: () => cwd,
    getDirectories: (path: string) => entries(path).directories,
    readDirectory: (
      path: string,
      extensions?: readonly string[],
      exclude?: readonly string[],
      include?: readonly string[],
      depth?: number,
    ) =>
      ts.matchFiles(
        path,
        extensions,
        exclude,
        include,
        true,
        cwd,
        depth,
        entries,
        (p) => p,
      ),
    realpath: (path: string) => path,
    exit: () => {},
    getModifiedTime: () => new Date(0),
    setTimeout,
    clearTimeout,
  };
}
