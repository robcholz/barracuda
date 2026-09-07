export interface WebEntry {
  id: string;
  title: string;
  module: string;
}
export type Cleanup = () => void;
export interface PortalModule {
  mount(
    root: HTMLElement,
    context: { signal: AbortSignal },
  ): void | Cleanup | Promise<void | Cleanup>;
}

export function parseEntries(value: unknown): WebEntry[] {
  if (!Array.isArray(value)) throw new Error("模块清单格式错误");
  const ids = new Set<string>();
  return value.map((item) => {
    if (
      !item ||
      typeof item.id !== "string" ||
      !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(item.id) ||
      item.id.length > 64 ||
      typeof item.title !== "string" ||
      typeof item.module !== "string"
    ) {
      throw new Error("模块清单格式错误");
    }
    const prefix = `/portal/assets/${item.id}/`;
    const relative = item.module.slice(prefix.length);
    if (
      ids.has(item.id) ||
      !item.module.startsWith(prefix) ||
      relative.length > 256 ||
      !relative
        .split("/")
        .every(
          (part: string) =>
            /^[a-zA-Z0-9_.-]+$/.test(part) && part !== "." && part !== "..",
        )
    ) {
      throw new Error("模块资源地址无效");
    }
    ids.add(item.id);
    return { id: item.id, title: item.title, module: item.module };
  });
}

/** Each navigation gets an isolated root and abort signal. Imports stay external to the shell bundle. */
export class ModuleSession {
  private controller?: AbortController;
  private cleanup?: Cleanup;

  close(): void {
    this.controller?.abort();
    this.controller = undefined;
    const cleanup = this.cleanup;
    this.cleanup = undefined;
    try {
      cleanup?.();
    } catch (error) {
      console.error("Module cleanup failed", error);
    }
  }

  async open(
    entry: WebEntry,
    root: HTMLElement,
    load: (url: string) => Promise<unknown> = (url) => import(url),
  ): Promise<void> {
    this.close();
    const controller = new AbortController();
    this.controller = controller;
    const module = (await load(entry.module)) as Partial<PortalModule> | null;
    if (controller.signal.aborted) return;
    if (!module || typeof module.mount !== "function")
      throw new Error("模块没有导出 mount()");
    const cleanup = await module.mount(root, { signal: controller.signal });
    if (cleanup !== undefined && typeof cleanup !== "function")
      throw new Error("模块清理函数无效");
    if (controller.signal.aborted) {
      try {
        cleanup?.();
      } catch (error) {
        console.error("Module cleanup failed", error);
      }
    } else {
      this.cleanup = typeof cleanup === "function" ? cleanup : undefined;
    }
  }
}
