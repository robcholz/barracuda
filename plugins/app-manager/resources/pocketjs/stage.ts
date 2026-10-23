// Stages the PocketJS compiler with Barracuda's patches to it.
//
// The vendored core carries changes upstream does not have yet
// (`crates/pocketjs-core/patches/`); the `.patch` files next to this module
// are the compiler's half of them. A build compiles with a copy of the
// sources it reads from the checkout at POCKETJS_ROOT, patched, so the
// checkout itself is never changed. The copy lives until the process exits.
import { cpSync, mkdtempSync, readdirSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

/** The PocketJS directories a compile reads. */
const SOURCES = ["contracts", "framework", "microts", "tools"];

/** Copies the compiler from `checkout`, applies the patches, and returns
 * the copy's root. Its dependencies stay in the checkout. */
export function stagePocketJS(checkout: string): string {
  const staged = mkdtempSync(join(tmpdir(), "barracuda-pocketjs-"));
  process.on("exit", () => rmSync(staged, { recursive: true, force: true }));
  for (const entry of readdirSync(checkout, { withFileTypes: true })) {
    if (entry.isFile()) cpSync(join(checkout, entry.name), join(staged, entry.name));
  }
  for (const directory of SOURCES) {
    cpSync(join(checkout, directory), join(staged, directory), { recursive: true, filter: (path) => !/[\\/](node_modules|target)$/.test(path) });
  }
  symlinkSync(join(checkout, "node_modules"), join(staged, "node_modules"));
  for (const patch of readdirSync(import.meta.dir).filter((file) => file.endsWith(".patch")).sort()) {
    const applied = Bun.spawnSync(["git", "apply", join(import.meta.dir, patch)], { cwd: staged, stdout: "inherit", stderr: "inherit" });
    if (applied.exitCode !== 0) {
      console.error(`pocketjs/${patch} does not apply to POCKETJS_ROOT; update it for this PocketJS.`);
      process.exit(1);
    }
  }
  return staged;
}
