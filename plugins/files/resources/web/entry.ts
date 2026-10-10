import {
  append,
  ICON_CHEVRON_DOWN,
  ICON_CHEVRON_RIGHT,
  ICON_DOWNLOAD,
  ICON_FILE,
  KIT_STRINGS,
  button,
  callDevice,
  deviceError,
  h,
  header,
  icon,
  kv,
  page,
  term,
  twoDigits,
  type DeviceResult,
  type Lang,
  type PortalContext,
  type PortalModule,
} from "../../../captive-portal/resources/web/ui";
import { STYLE } from "./style";

/** One `GET /api/files/list/<path>` entry (plugins/files/crates/plugin/src/api.rs `Entry`). */
export interface ListedEntry {
  name: string;
  type: "dir" | "file";
  size: number;
}

/** `GET /api/files/list/<path>` (`Listing`). */
export interface Listing {
  entries: ListedEntry[];
  truncated: boolean;
}

/** Lucide 0.469 icons (ISC) the kit does not carry. */
const ICONS = {
  drive:
    '<line x1="22" x2="2" y1="12" y2="12"></line><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"></path><line x1="6" x2="6.01" y1="16" y2="16"></line><line x1="10" x2="10.01" y1="16" y2="16"></line>',
  memory:
    '<path d="M6 19v-3"></path><path d="M10 19v-3"></path><path d="M14 19v-3"></path><path d="M18 19v-3"></path><path d="M8 11V9"></path><path d="M16 11V9"></path><path d="M12 11V9"></path><path d="M2 15h20"></path><path d="M2 7a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v1.1a2 2 0 0 0 0 3.837V17a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2v-5.1a2 2 0 0 0 0-3.837Z"></path>',
  package:
    '<path d="M11 21.73a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73z"></path><path d="M12 22V12"></path><path d="m3.3 7 7.703 4.734a2 2 0 0 0 1.994 0L20.7 7"></path><path d="m7.5 4.27 9 5.15"></path>',
  sd: '<path d="M6 22a2 2 0 0 1-2-2V6l4-4h10a2 2 0 0 1 2 2v16a2 2 0 0 1-2 2Z"></path><path d="M8 10V7.5"></path><path d="M12 6v4"></path><path d="M16 6v4"></path>',
  puzzle:
    '<path d="M15.39 4.39a1 1 0 0 0 1.68-.474 2.5 2.5 0 1 1 3.014 3.015 1 1 0 0 0-.474 1.68l1.683 1.682a2.414 2.414 0 0 1 0 3.414L19.61 15.39a1 1 0 0 1-1.68-.474 2.5 2.5 0 1 0-3.014 3.015 1 1 0 0 1 .474 1.68l-1.683 1.682a2.414 2.414 0 0 1-3.414 0L8.61 19.61a1 1 0 0 0-1.68.474 2.5 2.5 0 1 1-3.014-3.015 1 1 0 0 0 .474-1.68l-1.683-1.682a2.414 2.414 0 0 1 0-3.414L4.39 8.61a1 1 0 0 1 1.68.474 2.5 2.5 0 1 0 3.014-3.015 1 1 0 0 1-.474-1.68l1.683-1.682a2.414 2.414 0 0 1 3.414 0z"></path>',
  folder:
    '<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"></path>',
  folderOpen:
    '<path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2"></path>',
  folderPlus:
    '<path d="M12 10v6"></path><path d="M9 13h6"></path><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"></path>',
  text: '<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"></path><path d="M14 2v4a2 2 0 0 0 2 2h4"></path><path d="M10 9H8"></path><path d="M16 13H8"></path><path d="M16 17H8"></path>',
  code: '<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"></path><path d="M14 2v4a2 2 0 0 0 2 2h4"></path><path d="m10 13-2 2 2 2"></path><path d="m14 17 2-2-2-2"></path>',
  image:
    '<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"></path><path d="M14 2v4a2 2 0 0 0 2 2h4"></path><circle cx="10" cy="12" r="2"></circle><path d="m20 17-1.296-1.296a2.41 2.41 0 0 0-3.408 0L9 22"></path>',
  loader: '<path d="M21 12a9 9 0 1 1-6.219-8.56"></path>',
  upload:
    '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path><polyline points="17 8 12 3 7 8"></polyline><line x1="12" x2="12" y1="3" y2="15"></line>',
  more: '<circle cx="12" cy="12" r="1"></circle><circle cx="19" cy="12" r="1"></circle><circle cx="5" cy="12" r="1"></circle>',
  pencil:
    '<path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"></path><path d="m15 5 4 4"></path>',
  trash:
    '<path d="M3 6h18"></path><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"></path><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"></path><line x1="10" x2="10" y1="11" y2="17"></line><line x1="14" x2="14" y1="11" y2="17"></line>',
};

/** The page's copy: the design's PageFiles table, without modified times (the device keeps none). */
export const STRINGS = {
  zh: {
    title: "文件",
    lead: "查看和管理 Agent 与脚本共用的文件。",
    card: "存储卡",
    inserted: "已插入",
    notInserted: "未插入",
    tree: "文件树",
    path: "路径",
    media: "媒体",
    cache: "缓存",
    resources: "资源",
    removable: "存储卡",
    plugins: "插件",
    pluginTip: "由插件自己管理",
    readOnly: "只读",
    roTip: "随固件写入，升级时整体替换",
    temp: "临时",
    tempTip: "设备重启后清空",
    newFolder: "新建文件夹",
    upload: "上传文件",
    download: "下载",
    more: "更多操作",
    rename: "重命名",
    deleteFile: "删除文件",
    deleteFolder: "删除文件夹",
    confirmFile: "删除这个文件？",
    confirmFolder: "删除这个文件夹？",
    goneA: "「",
    goneB: "」会从设备上移除。",
    cancel: "取消",
    newName: "新名称",
    errEmpty: "请填写名称。",
    errSlash: "名称里不能有斜杠。",
    errDup: "这里已有同名的文件。",
    items: " 项",
    folderHint: "在左侧选择文件查看内容。",
    folderHintPhone: "在上方选择文件查看内容。",
    empty: "这个文件夹是空的",
    emptyHint: "上传的文件会出现在这里。",
    bigHint: "下载到电脑上查看。",
    noCard: "未插入存储卡",
    noCardHint: "插入存储卡后，它会出现在左侧。",
    uploading: "正在上传",
    uploaded: "已上传",
    deleted: "已删除",
    gone: "这个位置已不存在",
    removed: "存储卡已拔出",
    notEmpty: "文件夹里还有文件",
    refused: "设备拒绝了这个操作",
    failed: "设备没有完成这个操作",
  },
  en: {
    title: "Files",
    lead: "See and manage the files the agent and scripts share.",
    card: "SD card",
    inserted: "inserted",
    notInserted: "Not inserted",
    tree: "File tree",
    path: "Path",
    media: "Media",
    cache: "Cache",
    resources: "Resources",
    removable: "SD card",
    plugins: "Plugins",
    pluginTip: "Managed by the plugin itself",
    readOnly: "Read-only",
    roTip: "Written with the firmware and replaced on upgrade",
    temp: "Temporary",
    tempTip: "Cleared when the device restarts",
    newFolder: "New folder",
    upload: "Upload file",
    download: "Download",
    more: "More actions",
    rename: "Rename",
    deleteFile: "Delete file",
    deleteFolder: "Delete folder",
    confirmFile: "Delete this file?",
    confirmFolder: "Delete this folder?",
    goneA: "“",
    goneB: "” is removed from the device.",
    cancel: "Cancel",
    newName: "New name",
    errEmpty: "Enter a name.",
    errSlash: "A name can't contain a slash.",
    errDup: "Something here already has that name.",
    items: " items",
    folderHint: "Pick a file on the left to see it.",
    folderHintPhone: "Pick a file above to see it.",
    empty: "This folder is empty",
    emptyHint: "Files you upload show up here.",
    bigHint: "Download it to view it on a computer.",
    noCard: "No SD card",
    noCardHint: "Insert one and it shows up on the left.",
    uploading: "Uploading",
    uploaded: "Uploaded",
    deleted: "Deleted",
    gone: "That location is gone",
    removed: "The SD card was removed",
    notEmpty: "The folder still has files",
    refused: "The device refused that",
    failed: "The device didn't finish that",
  },
} as const;

type Strings = (typeof STRINGS)[Lang];
type PlaceId = "media" | "cache" | "resources" | "removable" | "plugins";

interface Place {
  id: PlaceId;
  icon: string;
  /** The real directory, or `null` while it has none (no card) or is assembled here (plugins). */
  path: string | null;
  readOnly: boolean;
  temp?: boolean;
}

/** One row of the tree: a place, a directory or a file. */
export interface Item {
  key: string;
  name: string;
  mono: boolean;
  /** The real path; `null` for a place without one and for a Plugin's own row. */
  path: string | null;
  dir: boolean;
  size: number;
  place: Place;
  parent: Item | null;
  /** `null` until read. */
  children: Item[] | null;
  truncated: boolean;
  open: boolean;
}

/** A Plugin's private trees, as `/plugins/<tree>/<id>`. */
const PLUGIN_TREES = ["data", "cache", "media", "resources"] as const;
/** Text larger than this is not drawn line by line. */
const TEXT_LIMIT = 64 * 1024;
const IMAGE_LIMIT = 2 * 1024 * 1024;
const TEXT_EXTENSIONS = ["txt", "md", "log"];
const IMAGE_EXTENSIONS = ["png", "jpg", "jpeg", "gif", "webp", "bmp"];
const LIST_TIMEOUT_MS = 15_000;
/** The phone layout, as the shell switches it (`PHONE_QUERY`): the tree stacks above the pane. */
const PHONE_QUERY = "(max-width: 719px)";

/** A real path in a route's URL: each name percent-encoded. */
export function encodePath(path: string): string {
  return path
    .split("/")
    .filter(Boolean)
    .map((name) => encodeURIComponent(name))
    .join("/");
}

export const rawUrl = (path: string) => `/api/files/raw/${encodePath(path)}`;

/** `812 B`, `2.4 KB`, `38 KB`, `2.3 MB`. */
export function formatSize(bytes: number): string {
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const digits = unit === 0 || value >= 10 ? 0 : 1;
  return `${value.toFixed(digits)} ${units[unit]}`;
}

/** Why a new name is refused, or `null`: names never hold a separator and are never `.` or `..`. */
export function nameError(
  name: string,
  siblings: readonly string[],
  current: string,
): "errEmpty" | "errSlash" | "errDup" | null {
  if (!name) return "errEmpty";
  if (/[/\\]/.test(name) || name === "." || name === "..") return "errSlash";
  if (name !== current && siblings.includes(name)) return "errDup";
  return null;
}

/** Groups `/plugins/<tree>/<id>` listings by Plugin: each ID with the trees it has. */
export function groupPlugins(
  trees: readonly (readonly [string, readonly ListedEntry[]])[],
): Map<string, string[]> {
  const plugins = new Map<string, string[]>();
  for (const [tree, entries] of trees)
    for (const entry of entries) {
      if (entry.type !== "dir") continue;
      const list = plugins.get(entry.name) ?? [];
      list.push(tree);
      plugins.set(entry.name, list);
    }
  return new Map([...plugins].sort(([a], [b]) => a.localeCompare(b)));
}

/** A tree row's trailing label keeps to one line beside a truncated name. */
function nowrap(element: HTMLElement): HTMLElement {
  element.style.cssText = "flex:none;white-space:nowrap";
  return element;
}

const extension = (name: string) =>
  name.includes(".") ? name.slice(name.lastIndexOf(".") + 1).toLowerCase() : "";

function fileIcon(name: string): string {
  const kind = extension(name);
  if (IMAGE_EXTENSIONS.includes(kind)) return ICONS.image;
  if (TEXT_EXTENSIONS.includes(kind)) return ICONS.text;
  return kind ? ICONS.code : ICON_FILE;
}

function item(
  parent: Item,
  entry: ListedEntry,
  previous?: Map<string, Item>,
): Item {
  const path = `${parent.path}/${entry.name}`;
  const old = previous?.get(path);
  return {
    key: path,
    name: entry.name,
    mono: true,
    path,
    dir: entry.type === "dir",
    size: entry.size,
    place: parent.place,
    parent,
    children: old && entry.type === "dir" ? old.children : null,
    truncated: old?.truncated ?? false,
    open: old?.open ?? false,
  };
}

class Browser {
  private readonly t: Strings;
  private readonly places: Item[];
  private selected: Item;
  private menu: "closed" | "menu" | "confirm" = "closed";
  private renaming: { value: string; error: string } | null = null;
  private uploading: { folder: Item; name: string } | null = null;
  private cards: string[] = [];
  /** The open file's contents, once read. */
  private preview:
    | { key: string; kind: "text"; lines: string[] }
    | { key: string; kind: "image"; url: string }
    | { key: string; kind: "binary" }
    | null = null;
  private readonly treeSlot = h("nav", { class: "bc-tree" });
  private readonly paneSlot = h("div", { class: "bc-pane" });
  private readonly cardSlot = h("span");
  private readonly picker = h("input", { type: "file", hidden: true });

  constructor(private readonly context: PortalContext) {
    this.t = STRINGS[context.lang];
    const place = (
      id: PlaceId,
      icon: string,
      path: string | null,
      readOnly: boolean,
      temp = false,
    ): Item => ({
      key: `place:${id}`,
      name: this.t[id],
      mono: false,
      path,
      dir: true,
      size: 0,
      place: { id, icon, path, readOnly, temp },
      parent: null,
      children: null,
      truncated: false,
      open: false,
    });
    this.places = [
      place("media", ICONS.drive, "/workspace/media", false),
      place("cache", ICONS.memory, "/workspace/cache", false, true),
      place("resources", ICONS.package, "/workspace/resources", true),
      place("removable", ICONS.sd, null, false),
      place("plugins", ICONS.puzzle, null, true),
    ];
    this.selected = this.places[0];
    this.treeSlot.setAttribute("aria-label", this.t.tree);
    this.picker.addEventListener("change", () => {
      const file = this.picker.files?.[0];
      this.picker.value = "";
      if (file) void this.upload(file);
    });
    const close = (event: Event) => {
      if (this.menu === "closed") return;
      const target = event.target as Element | null;
      if (
        event instanceof KeyboardEvent
          ? event.key === "Escape"
          : !target?.closest(".bc-menu-anchor")
      ) {
        this.menu = "closed";
        this.renderPane();
      }
    };
    document.addEventListener("click", close, { signal: context.signal });
    document.addEventListener("keydown", close, { signal: context.signal });
    context.signal.addEventListener("abort", () => this.dropPreview());
  }

  element(): HTMLElement {
    const { lang } = this.context;
    const t = this.t;
    const frame = h(
      "section",
      { class: "bc-frame", "aria-label": t.title },
      this.treeSlot,
      this.paneSlot,
    );
    frame.style.cssText =
      "display:flex;flex-wrap:wrap;align-items:stretch;min-height:600px";
    this.cardSlot.textContent = "—";
    return page(
      header(
        {
          title: t.title,
          lead: t.lead,
          extra: kv([[t.card, this.cardSlot]], lang, { live: true }),
          figure: "files",
        },
        lang,
      ),
      frame,
      this.picker,
    );
  }

  async start() {
    this.render();
    await Promise.all([this.findCards(), this.open(this.places[0])]);
  }

  // ---- reading -----------------------------------------------------------

  private async list(path: string): Promise<DeviceResult<Listing>> {
    return callDevice<Listing>(
      this.context,
      `/api/files/list/${encodePath(path)}`,
      { timeoutMs: LIST_TIMEOUT_MS },
    );
  }

  /** Mounted cards: one card is the place itself, several are its folders. */
  private async findCards() {
    const result = await this.list("/workspace/removable");
    if (result.kind === "aborted") return;
    this.cards =
      result.kind === "ok" && result.data
        ? result.data.entries.filter((e) => e.type === "dir").map((e) => e.name)
        : [];
    const removable = this.places[3];
    const path =
      this.cards.length === 1
        ? `/workspace/removable/${this.cards[0]}`
        : this.cards.length
          ? "/workspace/removable"
          : null;
    removable.path = path;
    removable.place.path = path;
    removable.children = null;
    this.cardSlot.replaceChildren(
      ...(this.cards.length
        ? [
            h("span", { class: "bc-mono" }, this.cards.join(", ")),
            ` · ${this.t.inserted}`,
          ]
        : [this.t.notInserted]),
    );
    this.render();
  }

  /** Reads a directory's children, keeping what is known of those it still has. */
  private async read(folder: Item): Promise<boolean> {
    if (folder.place.id === "plugins" && folder.parent === null)
      return this.readPlugins(folder);
    if (!folder.path) return false;
    const result = await this.list(folder.path);
    if (result.kind === "aborted") return false;
    if (result.kind !== "ok" || !result.data) {
      this.fail(result);
      return false;
    }
    const previous = new Map(
      (folder.children ?? []).map((child) => [child.key, child]),
    );
    folder.children = result.data.entries.map((entry) =>
      item(folder, entry, previous),
    );
    folder.truncated = result.data.truncated;
    return true;
  }

  /** 插件: every Plugin with a private tree, then the trees it has. */
  private async readPlugins(root: Item): Promise<boolean> {
    const results = await Promise.all(
      PLUGIN_TREES.map((tree) => this.list(`/plugins/${tree}`)),
    );
    if (results.some((result) => result.kind === "aborted")) return false;
    const trees = PLUGIN_TREES.map(
      (tree, index) =>
        [
          tree,
          (results[index].kind === "ok" && results[index].data?.entries) || [],
        ] as const,
    );
    const previous = new Map(
      (root.children ?? []).map((child) => [child.key, child]),
    );
    root.children = [...groupPlugins(trees)].map(([id, found]) => {
      const key = `plugin:${id}`;
      const plugin: Item = {
        key,
        name: id,
        mono: true,
        path: null,
        dir: true,
        size: 0,
        place: root.place,
        parent: root,
        children: [],
        truncated: false,
        open: previous.get(key)?.open ?? false,
      };
      const known = new Map(
        (previous.get(key)?.children ?? []).map((child) => [child.key, child]),
      );
      plugin.children = found.map((tree) => {
        const path = `/plugins/${tree}/${id}`;
        return {
          key: path,
          name: tree,
          mono: true,
          path,
          dir: true,
          size: 0,
          place: root.place,
          parent: plugin,
          children: known.get(path)?.children ?? null,
          truncated: false,
          open: known.get(path)?.open ?? false,
        };
      });
      return plugin;
    });
    return true;
  }

  // ---- navigation --------------------------------------------------------

  private async open(target: Item) {
    if (this.selected !== target) this.dropPreview();
    this.selected = target;
    this.menu = "closed";
    this.renaming = null;
    for (let at = target.dir ? target : target.parent; at; at = at.parent)
      at.open = true;
    this.render();
    if (target.dir) {
      if (target.children === null && (await this.read(target))) this.render();
    } else await this.loadPreview(target);
  }

  private toggle(target: Item) {
    target.open = !target.open;
    this.renderTree();
  }

  private dropPreview() {
    if (this.preview?.kind === "image") URL.revokeObjectURL(this.preview.url);
    this.preview = null;
  }

  private async loadPreview(file: Item) {
    if (!file.path) return;
    const kind = extension(file.name);
    const image = IMAGE_EXTENSIONS.includes(kind);
    if (file.size > (image ? IMAGE_LIMIT : TEXT_LIMIT)) {
      this.preview = { key: file.key, kind: "binary" };
      this.renderPane();
      return;
    }
    let bytes: Blob;
    try {
      const response = await fetch(rawUrl(file.path), {
        signal: this.context.signal,
        cache: "no-store",
        redirect: "error",
      });
      if (!response.ok) throw new Error(String(response.status));
      bytes = await response.blob();
    } catch {
      if (this.context.signal.aborted || this.selected !== file) return;
      this.preview = { key: file.key, kind: "binary" };
      this.renderPane();
      return;
    }
    if (this.selected !== file) return;
    if (image) {
      this.preview = {
        key: file.key,
        kind: "image",
        url: URL.createObjectURL(bytes),
      };
    } else {
      try {
        const text = new TextDecoder("utf-8", { fatal: true }).decode(
          await bytes.arrayBuffer(),
        );
        this.preview = text.includes("\0")
          ? { key: file.key, kind: "binary" }
          : { key: file.key, kind: "text", lines: text.split("\n") };
      } catch {
        this.preview = { key: file.key, kind: "binary" };
      }
    }
    this.renderPane();
  }

  // ---- changes -----------------------------------------------------------

  private folder(): Item {
    return this.selected.dir
      ? this.selected
      : (this.selected.parent ?? this.selected);
  }

  private writable(target: Item): boolean {
    return !target.place.readOnly && target.path !== null;
  }

  private fail(result: DeviceResult<unknown>) {
    if (result.kind === "aborted" || result.kind === "ok") return;
    const t = this.t;
    if (result.kind === "offline") {
      this.context.toast({
        kind: "error",
        title: KIT_STRINGS[this.context.lang].noReply,
      });
      return;
    }
    const { status, error } = result.error;
    const title =
      status === 404
        ? t.gone
        : status === 503
          ? t.removed
          : error === "exists"
            ? t.errDup.replace(/[。.]$/, "")
            : error === "not_empty"
              ? t.notEmpty
              : status < 500
                ? t.refused
                : t.failed;
    this.context.toast({ kind: "error", title, code: String(status) });
  }

  private change(route: "mkdir" | "rename" | "delete", body: unknown) {
    return callDevice(this.context, `/api/files/${route}`, {
      method: "POST",
      body,
    });
  }

  private async upload(file: File) {
    const folder = this.folder();
    if (!folder.path || this.uploading) return;
    const names = (folder.children ?? []).map((child) => child.name);
    if (names.includes(file.name)) {
      this.context.toast({
        kind: "error",
        title: this.t.errDup.replace(/[。.]$/, ""),
        code: file.name,
      });
      return;
    }
    this.uploading = { folder, name: file.name };
    folder.open = true;
    this.render();
    let result: DeviceResult<unknown>;
    try {
      const response = await fetch(
        `/api/files/upload/${encodePath(`${folder.path}/${file.name}`)}`,
        {
          method: "PUT",
          body: file,
          signal: this.context.signal,
          cache: "no-store",
          redirect: "error",
        },
      );
      result = response.ok
        ? { kind: "ok", status: response.status, data: null }
        : {
            kind: "error",
            error: await deviceError(response),
          };
    } catch {
      result = this.context.signal.aborted
        ? { kind: "aborted" }
        : { kind: "offline" };
    }
    if (result.kind === "aborted") return;
    this.uploading = null;
    if (result.kind === "ok") {
      this.context.toast({
        kind: "success",
        title: this.t.uploaded,
        code: file.name,
      });
      await this.read(folder);
    } else this.fail(result);
    this.render();
  }

  private async newFolder() {
    const folder = this.folder();
    if (!folder.path) return;
    const names = new Set((folder.children ?? []).map((child) => child.name));
    let name = "untitled";
    for (let index = 2; names.has(name); index += 1) name = `untitled-${index}`;
    const result = await this.change("mkdir", {
      path: `${folder.path}/${name}`,
    });
    if (result.kind !== "ok") return this.fail(result);
    await this.read(folder);
    const created = folder.children?.find((child) => child.name === name);
    if (!created) return this.render();
    await this.open(created);
    this.renaming = { value: name, error: "" };
    this.renderPane();
  }

  private async rename() {
    const target = this.selected;
    const parent = target.parent;
    if (!this.renaming || !parent || !target.path || !parent.path) return;
    const value = this.renaming.value.trim();
    const siblings = (parent.children ?? []).map((child) => child.name);
    const refused = nameError(value, siblings, target.name);
    if (refused) {
      this.renaming = { value, error: this.t[refused] };
      return this.renderPane();
    }
    if (value === target.name) {
      this.renaming = null;
      return this.renderPane();
    }
    const to = `${parent.path}/${value}`;
    const result = await this.change("rename", { from: target.path, to });
    if (result.kind === "error" && result.error.error === "exists") {
      this.renaming = { value, error: this.t.errDup };
      return this.renderPane();
    }
    if (result.kind !== "ok") return this.fail(result);
    await this.read(parent);
    const renamed = parent.children?.find((child) => child.name === value);
    await this.open(renamed ?? parent);
  }

  private async remove() {
    const target = this.selected;
    const parent = target.parent;
    if (!target.path || !parent) return;
    const result = await this.change("delete", { path: target.path });
    this.menu = "closed";
    if (result.kind !== "ok") {
      this.renderPane();
      return this.fail(result);
    }
    this.context.toast({
      kind: "success",
      title: this.t.deleted,
      code: target.name,
    });
    await this.read(parent);
    await this.open(parent);
  }

  // ---- drawing -----------------------------------------------------------

  private render() {
    this.renderTree();
    this.renderPane();
  }

  private renderTree() {
    const t = this.t;
    const rows: HTMLElement[] = [];
    const row = (target: Item, depth: number) => {
      const unread = target.children === null;
      const uploadingHere = this.uploading?.folder === target;
      const kids =
        target.dir &&
        (unread || (target.children?.length ?? 0) > 0 || uploadingHere);
      const open = target.dir && target.open && !unread && kids;
      const current = this.selected === target;
      const missing =
        target.place.id === "removable" &&
        target.parent === null &&
        !target.path;
      const spacer = h("span");
      spacer.style.cssText = `flex:none;width:${depth * 16}px`;
      const chevron =
        kids && !missing
          ? icon(
              open ? ICON_CHEVRON_DOWN : ICON_CHEVRON_RIGHT,
              undefined,
              "bc-muted",
            )
          : h("span");
      if (!(kids && !missing)) chevron.style.cssText = "flex:none;width:16px";
      const mark =
        target.parent === null
          ? icon(target.place.icon)
          : target.dir
            ? icon(open ? ICONS.folderOpen : ICONS.folder)
            : icon(fileIcon(target.name), undefined, "bc-muted");
      const detail =
        target.parent === null && (target.place.readOnly || missing)
          ? nowrap(
              h(
                "span",
                { class: "bc-caption bc-muted" },
                target.place.readOnly ? t.readOnly : t.notInserted,
              ),
            )
          : null;
      rows.push(
        h(
          "a",
          {
            class: "bc-nav-item",
            href: "#files",
            "aria-current": current ? "page" : "false",
            "aria-expanded": target.dir && kids ? String(open) : undefined,
            onclick: (event: Event) => {
              event.preventDefault();
              if (current && target.dir) {
                if (target.children === null) void this.open(target);
                else this.toggle(target);
              } else void this.open(target);
            },
          },
          spacer,
          chevron,
          mark,
          h(
            "span",
            { class: `bc-tree__name${target.mono ? " bc-mono" : ""}` },
            target.name,
          ),
          detail,
        ),
      );
      if (open)
        for (const child of target.children ?? []) row(child, depth + 1);
      if (this.uploading && uploadingHere && open) {
        const pad = h("span");
        pad.style.cssText = `flex:none;width:${(depth + 1) * 16 + 16}px`;
        rows.push(
          h(
            "a",
            { class: "bc-nav-item", "aria-disabled": "true" },
            pad,
            icon(ICONS.loader, undefined, "bc-muted bc-spinner"),
            h("span", { class: "bc-tree__name bc-mono" }, this.uploading.name),
            nowrap(h("span", { class: "bc-caption bc-shimmer" }, t.uploading)),
          ),
        );
      }
    };
    for (const place of this.places) row(place, 0);
    this.treeSlot.replaceChildren(...rows);
  }

  private crumbs(): HTMLElement {
    const t = this.t;
    const chain: Item[] = [];
    for (let at: Item | null = this.selected; at; at = at.parent)
      chain.unshift(at);
    const parts: HTMLElement[] = [];
    chain.forEach((step, index) => {
      const mono = step.mono ? " bc-mono" : "";
      if (index === chain.length - 1)
        parts.push(
          h(
            "span",
            { class: `bc-crumb--current${mono}`, "aria-current": "location" },
            step.name,
          ),
        );
      else
        parts.push(
          h(
            "button",
            {
              class: `bc-link bc-crumb${mono}`,
              type: "button",
              onclick: () => void this.open(step),
            },
            step.name,
          ),
          h("span", { class: "bc-crumb", "aria-hidden": "true" }, "/"),
        );
    });
    const place = this.selected.place;
    if (place.readOnly)
      parts.push(
        h(
          "span",
          { class: "bc-small bc-muted" },
          term(
            t.readOnly,
            place.id === "plugins" ? t.pluginTip : t.roTip,
            this.context.lang,
          ),
        ),
      );
    if (place.temp)
      parts.push(
        h(
          "span",
          { class: "bc-small bc-muted" },
          term(t.temp, t.tempTip, this.context.lang),
        ),
      );
    const nav = h("nav", { "aria-label": t.path }, parts);
    nav.style.cssText =
      "display:flex;align-items:center;flex-wrap:wrap;gap:6px;min-width:0";
    return nav;
  }

  private count(folder: Item): string {
    const count = folder.children?.length ?? 0;
    return `${twoDigits(count)}${folder.truncated ? "+" : ""}`;
  }

  private renderPane() {
    const t = this.t;
    const { lang } = this.context;
    const target = this.selected;
    const file = target.dir ? null : target;
    const missing = target.place.id === "removable" && !target.path;
    if (missing) {
      this.paneSlot.replaceChildren(this.emptyState(t.noCard, t.noCardHint));
      return;
    }
    const writable = this.writable(target);
    const folderView = !file;
    const meta = h(
      "span",
      { class: "bc-caption bc-muted" },
      file
        ? h("span", { class: "bc-mono" }, formatSize(file.size))
        : target.children
          ? [h("span", { class: "bc-mono" }, this.count(target)), t.items]
          : "—",
    );
    const title = h("div", null, this.crumbs(), meta);
    title.style.cssText =
      "flex:1 1 240px;min-width:0;display:flex;flex-direction:column;gap:2px";
    const actions: (HTMLElement | null)[] = [];
    if (folderView && writable && !this.renaming) {
      actions.push(
        button(t.newFolder, lang, {
          variant: "outline",
          size: "sm",
          icon: ICONS.folderPlus,
          onClick: () => void this.newFolder(),
        }),
      );
      const upload = button(t.upload, lang, {
        size: "sm",
        icon: ICONS.upload,
        onClick: () => this.picker.click(),
      });
      if (this.uploading) upload.setAttribute("disabled", "");
      actions.push(upload);
    }
    if (file?.path)
      actions.push(
        h(
          "a",
          {
            class: "bc-button bc-button--outline bc-button--sm",
            href: rawUrl(file.path),
            download: file.name,
          },
          icon(ICON_DOWNLOAD),
          t.download,
        ),
      );
    if (writable && target.parent && !this.renaming)
      actions.push(this.moreMenu(target));
    const bar = h("div", { class: "bc-pane__bar" }, title, actions);
    this.paneSlot.replaceChildren();
    append(
      this.paneSlot,
      bar,
      this.renaming ? this.renamePanel() : null,
      h(
        "div",
        { class: "bc-pane__body" },
        file ? this.fileBody(file) : this.folderBody(target),
      ),
    );
  }

  private moreMenu(target: Item): HTMLElement {
    const t = this.t;
    const file = !target.dir;
    const canDelete = file || target.children?.length === 0;
    const deleteLabel = file ? t.deleteFile : t.deleteFolder;
    const trigger = h(
      "button",
      {
        class: "bc-button bc-button--outline bc-button--sm bc-button--icon",
        type: "button",
        "aria-label": t.more,
        "aria-haspopup": "menu",
        "aria-expanded": String(this.menu !== "closed"),
        onclick: () => {
          this.menu = this.menu === "closed" ? "menu" : "closed";
          this.renderPane();
        },
      },
      icon(ICONS.more),
      h("span", { class: "bc-tooltip", "aria-hidden": "true" }, t.more),
    );
    let popup: HTMLElement | null = null;
    if (this.menu === "menu")
      popup = h(
        "div",
        { class: "bc-menu", role: "menu" },
        h(
          "button",
          {
            class: "bc-menu-item",
            type: "button",
            role: "menuitem",
            onclick: () => {
              this.menu = "closed";
              this.renaming = { value: target.name, error: "" };
              this.renderPane();
            },
          },
          icon(ICONS.pencil),
          t.rename,
        ),
        canDelete
          ? h(
              "button",
              {
                class: "bc-menu-item bc-menu-item--destructive",
                type: "button",
                role: "menuitem",
                onclick: () => {
                  this.menu = "confirm";
                  this.renderPane();
                },
              },
              icon(ICONS.trash),
              deleteLabel,
            )
          : null,
      );
    else if (this.menu === "confirm") {
      const confirmTitle = file ? t.confirmFile : t.confirmFolder;
      const text = h(
        "span",
        null,
        h("span", { class: "bc-option-title" }, confirmTitle),
        h(
          "span",
          { class: "bc-small bc-muted" },
          t.goneA,
          h("span", { class: "bc-mono" }, target.name),
          t.goneB,
        ),
      );
      text.style.cssText = "display:flex;flex-direction:column;gap:4px";
      const buttons = h(
        "span",
        null,
        button(t.cancel, this.context.lang, {
          variant: "outline",
          size: "sm",
          onClick: () => {
            this.menu = "closed";
            this.renderPane();
          },
        }),
        button(deleteLabel, this.context.lang, {
          variant: "danger",
          size: "sm",
          icon: ICONS.trash,
          onClick: () => void this.remove(),
        }),
      );
      buttons.style.cssText = "display:flex;justify-content:flex-end;gap:8px";
      popup = h(
        "div",
        {
          class: "bc-menu bc-menu--confirm",
          role: "alertdialog",
          "aria-label": confirmTitle,
        },
        text,
        buttons,
      );
    }
    return h("span", { class: "bc-menu-anchor" }, trigger, popup);
  }

  private renamePanel(): HTMLElement {
    const t = this.t;
    const state = this.renaming ?? { value: "", error: "" };
    const input = h("input", {
      class: "bc-input bc-input--mono",
      type: "text",
      value: state.value,
      "aria-invalid": state.error ? "true" : "false",
      oninput: (event: Event) => {
        if (this.renaming)
          this.renaming.value = (event.target as HTMLInputElement).value;
      },
      onkeydown: (event: Event) => {
        if ((event as KeyboardEvent).key === "Enter") void this.rename();
      },
    });
    const field = h(
      "label",
      { class: "bc-field" },
      h("span", { class: "bc-label" }, t.newName),
      input,
      state.error
        ? h("span", { class: "bc-hint bc-hint--error" }, state.error)
        : null,
    );
    field.style.flex = "1 1 260px";
    queueMicrotask(() => {
      input.focus();
      input.select();
    });
    return h(
      "div",
      { class: "bc-pane__section" },
      h(
        "div",
        { class: "bc-expanded" },
        h(
          "div",
          { class: "bc-expanded__panel" },
          field,
          button(t.cancel, this.context.lang, {
            variant: "outline",
            onClick: () => {
              this.renaming = null;
              this.renderPane();
            },
          }),
          button(t.rename, this.context.lang, {
            onClick: () => void this.rename(),
          }),
        ),
      ),
    );
  }

  private emptyState(
    title: string,
    hint: string | null,
    ...extra: HTMLElement[]
  ) {
    const box = h(
      "div",
      { class: "bc-empty" },
      h("h2", { class: "bc-title" }, title),
      hint ? h("span", { class: "bc-small bc-muted" }, hint) : null,
      ...extra,
    );
    box.style.cssText = "flex:1 1 auto;justify-content:center;gap:12px";
    return box;
  }

  private folderBody(folder: Item): HTMLElement {
    const t = this.t;
    if (!folder.children) return this.emptyState("—", null);
    const count = folder.children.length;
    if (count)
      return this.emptyState(
        `${this.count(folder)}${t.items}`,
        matchMedia(PHONE_QUERY).matches ? t.folderHintPhone : t.folderHint,
      );
    return this.emptyState(t.empty, this.writable(folder) ? t.emptyHint : null);
  }

  private fileBody(file: Item): HTMLElement {
    const t = this.t;
    const preview = this.preview?.key === file.key ? this.preview : null;
    if (!preview) return this.emptyState("—", null);
    if (preview.kind === "text") {
      const lines = h("div", { class: "bc-lines" });
      preview.lines.forEach((line, index) => {
        lines.append(
          h(
            "span",
            { class: "bc-lines__n bc-mono bc-caption bc-muted" },
            index + 1,
          ),
          h("span", { class: "bc-lines__text bc-mono bc-small" }, line || " "),
        );
      });
      return lines;
    }
    if (preview.kind === "image") {
      const image = h("img", { src: preview.url, alt: file.name });
      image.style.cssText =
        "max-width:100%;max-height:480px;object-fit:contain";
      const frame = h("div", { class: "bc-frame" }, image);
      frame.style.cssText =
        "flex:1 1 auto;min-height:320px;display:flex;align-items:center;justify-content:center;padding:16px";
      const wrap = h("div", null, frame);
      wrap.style.cssText = "flex:1 1 auto;padding:20px;display:flex";
      return wrap;
    }
    const title = h("h2", { class: "bc-title bc-mono" }, file.name);
    const box = this.emptyState("", t.bigHint);
    box.replaceChild(title, box.firstChild as Node);
    if (file.path)
      box.append(
        h(
          "a",
          {
            class: "bc-button bc-button--outline bc-button--sm",
            href: rawUrl(file.path),
            download: file.name,
          },
          icon(ICON_DOWNLOAD),
          t.download,
          " ",
          h("span", { class: "bc-mono" }, formatSize(file.size)),
        ),
      );
    return box;
  }
}

export const mount: PortalModule["mount"] = (root, context) => {
  if (context.signal.aborted) return;
  const style = h("style", null, STYLE);
  document.head.append(style);
  const browser = new Browser(context);
  root.replaceChildren(browser.element());
  void browser.start();
  return () => {
    root.replaceChildren();
    style.remove();
  };
};
