import { afterEach, beforeEach, expect, test } from "bun:test";
import {
  installBrowser,
  settle,
} from "../../../../captive-portal/resources/web/tests/browser";
import type { PortalContext } from "../../../../captive-portal/resources/web/ui";
import { mount } from "../entry";

let browser: ReturnType<typeof installBrowser>;
let sockets: Socket[];
let sent: string[];
const previous = {
  WebSocket: globalThis.WebSocket,
  XMLHttpRequest: globalThis.XMLHttpRequest,
};
/** Bun's own Event: the fakes are Bun EventTargets, while happy-dom replaces the global. */
const NativeEvent = globalThis.Event;

class Socket extends EventTarget {
  static OPEN = 1;
  readyState = 0;
  bufferedAmount = 0;
  constructor(public url: string | URL) {
    super();
    sockets.push(this);
  }
  send(text: string) {
    if (!text.includes('"control"')) sent.push(text);
  }
  close() {
    this.readyState = 3;
  }
  open() {
    this.readyState = 1;
    this.dispatchEvent(new NativeEvent("open"));
  }
  emit(event: string, data: unknown) {
    this.dispatchEvent(
      new MessageEvent("message", {
        data: `id: 1\nevent: ${event}\ndata: ${JSON.stringify(data)}\n\n`,
      }),
    );
  }
}

/** An upload the page started: its request, and the answers the test gives it. */
class Upload {
  static all: Upload[] = [];
  method = "";
  url = "";
  body: unknown = null;
  status = 0;
  responseText = "";
  aborted = false;
  upload = new EventTarget();
  private events = new EventTarget();
  constructor() {
    Upload.all.push(this);
  }
  open(method: string, url: string | URL) {
    this.method = method;
    this.url = String(url);
  }
  send(body: unknown) {
    this.body = body;
  }
  abort() {
    this.aborted = true;
  }
  addEventListener(name: string, listener: EventListener) {
    this.events.addEventListener(name, listener);
  }
  progress(loaded: number, total: number) {
    this.upload.dispatchEvent(
      Object.assign(new NativeEvent("progress"), {
        lengthComputable: true,
        loaded,
        total,
      }),
    );
  }
  answer(status: number, body: unknown) {
    this.status = status;
    this.responseText = JSON.stringify(body);
    this.events.dispatchEvent(new NativeEvent("load"));
  }
}

beforeEach(() => {
  browser = installBrowser();
  sockets = [];
  sent = [];
  Upload.all = [];
  Object.assign(globalThis, { WebSocket: Socket, XMLHttpRequest: Upload });
});
afterEach(async () => {
  Object.assign(globalThis, previous);
  await browser.close();
});

function open() {
  const controller = new AbortController();
  const context: PortalContext = {
    signal: controller.signal,
    lang: "zh",
    toast: () => {},
    navigate: () => {},
    has: () => true,
    status: () => null,
    refreshStatus: async () => {},
    badge: () => {},
  };
  const root = browser.document.createElement("div");
  browser.document.body.append(root);
  mount(root, context);
  const ws = sockets.at(-1)!;
  ws.open();
  const input = root.querySelector("textarea")!;
  const picker = root.querySelector<HTMLInputElement>("input[type=file]")!;
  const pick = async (...files: File[]) => {
    Object.defineProperty(picker, "files", {
      value: files,
      configurable: true,
    });
    picker.dispatchEvent(new browser.window.Event("change"));
    await settle();
  };
  const type = (text: string) => {
    input.value = text;
    input.dispatchEvent(new browser.window.Event("input"));
  };
  const submit = () =>
    root
      .querySelector("form")!
      .dispatchEvent(new browser.window.Event("submit", { cancelable: true }));
  const button = (label: string, within: ParentNode = root) =>
    [...within.querySelectorAll<HTMLElement>("button, a")].find(
      (node) =>
        node.getAttribute("aria-label") === label ||
        node.textContent?.trim() === label,
    )!;
  const send = () =>
    root.querySelector<HTMLButtonElement>("button[type=submit]")!;
  const chips = () => [...root.querySelectorAll<HTMLElement>(".bc-attachment")];
  const hints = () =>
    [
      ...root.querySelectorAll<HTMLElement>(
        ".bc-chat-dock .bc-hint--error:not([hidden])",
      ),
    ]
      .filter((node) => node.textContent)
      .map((node) => node.textContent);
  return {
    root,
    controller,
    ws,
    input,
    pick,
    type,
    submit,
    button,
    send,
    chips,
    hints,
  };
}

const png = (name = "cat.png", size = 2048) =>
  new File([new Uint8Array(size)], name, { type: "image/png" });
const pdf = () =>
  new File([new Uint8Array(3)], "notes.pdf", { type: "application/pdf" });
const ok = (id: string) => ({ id, kind: "image", mime_type: "", size: 1 });

test("files wait in the composer while they upload; failures say so and retry", async () => {
  const { root, pick, type, submit, button, send, chips, hints } = open();
  // the paperclip comes first among the composer's actions
  const attach = button("添加附件") as HTMLButtonElement;
  expect(attach.className).toBe("bc-button bc-button--ghost bc-button--icon");
  expect(root.querySelector(".bc-composer__actions")!.firstElementChild).toBe(
    attach,
  );
  expect(attach.querySelector(".bc-tooltip")?.textContent).toBe("添加附件");
  const row = root.querySelector<HTMLElement>(".bc-attachments")!;
  expect(row.hidden).toBe(true);

  await pick(png(), pdf());
  // the row sits after the composer's bars, before its textarea row
  expect(row.hidden).toBe(false);
  expect(row.nextElementSibling?.className).toBe("bc-drop");
  expect(chips()).toHaveLength(2);
  const [image, file] = chips();
  expect(image.className).toBe("bc-attachment bc-attachment--busy");
  expect(image.querySelector("img")!.getAttribute("src")).toStartWith("blob:");
  expect(file.className).toBe(
    "bc-attachment bc-attachment--file bc-attachment--busy",
  );
  expect(
    file.querySelector(".bc-attachment__name > .bc-small")?.textContent,
  ).toBe("notes.pdf");
  expect(
    file.querySelector(".bc-attachment__name > .bc-caption.bc-mono")
      ?.textContent,
  ).toBe("PDF · 3 B");
  expect(
    image
      .querySelector(".bc-attachment__state .bc-spinner")
      ?.getAttribute("aria-label"),
  ).toBe("上传中");
  expect(button("移除附件", image).className).toBe(
    "bc-icon-button bc-attachment__remove",
  );
  // the type and the name travel in the path, since an upload has no headers
  const to = (name: string) =>
    Upload.all.filter((each) => each.url.endsWith(`/${name}`)).at(-1)!;
  const [first, second] = [to("cat.png"), to("notes.pdf")];
  expect(first.method).toBe("PUT");
  expect(first.url).toBe("http://localhost/api/web/upload/image%2Fpng/cat.png");
  expect(second.url).toEndWith("/api/web/upload/application%2Fpdf/notes.pdf");
  first.progress(512, 2048);
  expect(
    chips()[0].querySelector<HTMLElement>(".bc-attachment__progress > i")!.style
      .width,
  ).toBe("25%");
  // nothing goes out while a file uploads
  type("看看这个");
  expect(send().disabled).toBe(true);

  first.answer(201, ok("img-1"));
  second.answer(500, { error: "io" });
  expect(chips()[0].className).toBe("bc-attachment");
  expect(chips()[1].className).toContain("bc-attachment--error");
  expect(hints()).toEqual(["一个文件没有上传成功，点它重试或移除后再发送。"]);
  expect(send().disabled).toBe(true);
  const retry = button("重试上传", chips()[1]);
  expect(retry.className).toBe("bc-icon-button bc-attachment__retry");
  retry.click();
  await settle();
  expect(chips()[1].className).toContain("bc-attachment--busy");
  expect(hints()).toEqual([]);
  expect(Upload.all).toHaveLength(3);
  to("notes.pdf").answer(201, ok("pdf-1"));
  expect(send().disabled).toBe(false);

  // files alone are enough to send
  type("");
  expect(send().disabled).toBe(false);
  submit();
  expect(sent.map((frame) => JSON.parse(frame))).toEqual([
    { text: "", attachments: ["img-1", "pdf-1"] },
  ]);
  expect(row.hidden).toBe(true);
  expect(chips()).toHaveLength(0);
  // the user turn shows them from the page's own copies, above where a bubble would be
  const turn = root.querySelector(".bc-turn--user")!;
  expect(turn.querySelector(".bc-bubble")).toBeNull();
  const shown = turn.querySelector<HTMLButtonElement>("button.bc-media")!;
  expect(shown.getAttribute("aria-label")).toBe("查看图片 cat.png");
  expect(shown.style.maxWidth).toBe("320px");
  expect(shown.querySelector("img")!.getAttribute("src")).toStartWith("blob:");
  expect(turn.querySelector(".bc-option-title")?.textContent).toBe("notes.pdf");
});

test("a refused file names the limit; at most four files, none while offline", async () => {
  const { pick, button, chips, hints, ws } = open();
  await pick(new File([new Uint8Array(9)], "big.zip", { type: "" }));
  expect(Upload.all[0].url).toEndWith(
    "/api/web/upload/application%2Foctet-stream/big.zip",
  );
  Upload.all[0].answer(413, { error: "too_large", limit: 8388608 });
  expect(hints()).toEqual(["「big.zip」超过 8 MB，请换一个更小的文件。"]);
  // a smaller file is the way out, not a retry
  expect(chips()[0].querySelector(".bc-attachment__retry")).toBeNull();
  button("移除附件", chips()[0]).click();
  expect(chips()).toHaveLength(0);
  expect(hints()).toEqual([]);

  const attach = button("添加附件") as HTMLButtonElement;
  await pick(
    png("1.png"),
    png("2.png"),
    png("3.png"),
    png("4.png"),
    png("5.png"),
  );
  expect(chips()).toHaveLength(4);
  expect(attach.disabled).toBe(true);
  button("移除附件", chips()[0]).click();
  expect(Upload.all.find((each) => each.url.endsWith("/1.png"))!.aborted).toBe(
    true,
  );
  expect(attach.disabled).toBe(false);
  ws.dispatchEvent(new NativeEvent("close"));
  expect(attach.disabled).toBe(true);
});

test("a drag over the page turns the composer into a drop zone; a pasted picture joins", async () => {
  const { root, input, chips } = open();
  const composer = root.querySelector<HTMLElement>(".bc-composer")!;
  const drop = root.querySelector<HTMLElement>(".bc-drop")!;
  const drag = (name: string, files: File[] = []) => {
    const event = new browser.window.Event(name, { cancelable: true });
    Object.defineProperty(event, "dataTransfer", {
      value: { types: ["Files"], files },
    });
    browser.window.dispatchEvent(event);
  };
  drag("dragenter");
  expect(composer.classList.contains("bc-composer--drop")).toBe(true);
  expect(drop.hidden).toBe(false);
  expect(drop.textContent).toBe("松开以添加附件");
  expect(drop.nextElementSibling?.nextElementSibling?.className).toBe(
    "bc-composer__row",
  );
  drag("drop", [pdf()]);
  await settle();
  expect(composer.classList.contains("bc-composer--drop")).toBe(false);
  expect(drop.hidden).toBe(true);
  expect(chips()).toHaveLength(1);

  const paste = new browser.window.Event("paste", { cancelable: true });
  Object.defineProperty(paste, "clipboardData", {
    value: { files: [png("pasted.png")] },
  });
  input.dispatchEvent(paste);
  await settle();
  expect(chips()).toHaveLength(2);
  expect(paste.defaultPrevented).toBe(true);
});

test("a queued message keeps its files and sends them when its turn comes", async () => {
  const { root, ws, pick, type, submit, button, chips } = open();
  type("一");
  submit();
  await pick(png());
  Upload.all[0].answer(201, ok("img-1"));
  type("二");
  submit();
  expect(chips()).toHaveLength(0);
  // editing it puts its files back as they are, uploaded already
  button("编辑", root.querySelector("[role=list]")!).click();
  expect(chips()).toHaveLength(1);
  expect(Upload.all).toHaveLength(1);
  submit();
  // the queue bar counts the files in mono, after a muted paperclip, before the text
  const bar = root.querySelector("[role=list] [role=listitem]")!;
  const text = bar.querySelector(".bc-composer__text")!;
  expect(text.textContent).toBe("二");
  expect(text.previousElementSibling?.textContent).toBe("1");
  expect(text.previousElementSibling?.className).toBe("bc-mono bc-muted");
  expect(
    text.previousElementSibling?.previousElementSibling?.classList.contains(
      "bc-muted",
    ),
  ).toBe(true);
  ws.emit("message.start", {
    kind: "reply",
    message_id: "web-1",
    reply_to: "web-in-1",
  });
  ws.emit("message.end", { error: null, message_id: "web-1" });
  expect(JSON.parse(sent.at(-1)!)).toEqual({
    text: "二",
    attachments: ["img-1"],
  });
  expect(root.querySelectorAll(".bc-turn--user")[1].textContent).toContain(
    "二",
  );
});

test("a frame the device refused says its files expired", () => {
  const { root, ws, type, submit } = open();
  type("一");
  submit();
  ws.emit("message.rejected", {
    error: "unknown_attachment",
    message_id: "web-in-1",
  });
  const turn = root.querySelector(".bc-turn--user")!;
  expect(turn.querySelector(".bc-status.bc-destructive")?.textContent).toBe(
    "附件已失效，这条消息没有发出",
  );
});

/** The Agent's media message, as the bridge sends it. */
function media(
  ws: Socket,
  kind: string,
  id: string,
  name: string,
  mime: string,
  size = 2048,
) {
  ws.emit(`message.${kind}`, {
    caption: null,
    filename: name,
    message_id: id,
    mime_type: mime,
    reply_to: "web-in-1",
    size,
    url: `/api/web/media/${id}`,
  });
}

test("Agent media: a picture, a grid with +N, audio, video, a file, and a picture gone", () => {
  const { root, ws, button } = open();
  media(ws, "image", "m-1", "one.png", "image/png");
  const single = root.querySelector<HTMLButtonElement>("button.bc-media")!;
  expect(single.getAttribute("aria-label")).toBe("查看图片 one.png");
  expect(single.style.maxWidth).toBe("440px");
  expect(single.querySelector("img")!.getAttribute("src")).toBe(
    "/api/web/media/m-1",
  );
  // pictures in a row of the turn share one grid; past four, the fourth says how many more
  for (const index of [2, 3, 4, 5, 6])
    media(ws, "image", `m-${index}`, `${index}.png`, "image/png");
  const grid = root.querySelector(".bc-media-grid")!;
  const cells = [...grid.children];
  expect(cells).toHaveLength(4);
  expect(cells[3].className).toBe("bc-media bc-media--more");
  expect(cells[3].getAttribute("aria-label")).toBe("查看另外 3 张图片");
  expect(
    cells[3].querySelector(".bc-media__more > .bc-title.bc-mono")?.textContent,
  ).toBe("+3");
  expect(root.querySelectorAll("button.bc-media")).toHaveLength(4);

  media(ws, "audio", "m-7", "voice.ogg", "audio/ogg");
  const audio = root.querySelector("audio")!;
  expect(audio.hidden).toBe(true);
  expect(audio.parentElement?.className).toBe("bc-track");
  const card = audio.closest(".bc-card")!;
  const play = button("播放", card);
  expect(play.className).toBe(
    "bc-button bc-button--outline bc-button--icon bc-button--sm",
  );
  expect(card.querySelector(".bc-small")?.textContent).toBe("voice.ogg");
  Object.defineProperty(audio, "duration", { value: 48, configurable: true });
  Object.defineProperty(audio, "currentTime", {
    value: 12,
    configurable: true,
  });
  audio.dispatchEvent(new browser.window.Event("timeupdate"));
  expect(card.querySelector(".bc-caption.bc-mono")?.textContent).toBe(
    "0:12 / 0:48",
  );
  expect(card.querySelector<HTMLElement>(".bc-track > i")!.style.width).toBe(
    "25%",
  );
  audio.dispatchEvent(new browser.window.Event("play"));
  expect(play.getAttribute("aria-label")).toBe("暂停");
  audio.dispatchEvent(new browser.window.Event("pause"));
  expect(play.getAttribute("aria-label")).toBe("播放");

  media(ws, "video", "m-8", "clip.mp4", "video/mp4");
  const box = root.querySelector(".bc-media.bc-media--video")!;
  const video = box.querySelector("video")!;
  expect(video.getAttribute("src")).toBe("/api/web/media/m-8#t=0.1");
  expect(video.controls).toBe(false);
  const start = button("播放视频", box);
  expect(start.className).toContain("bc-media__play");
  start.click();
  expect(box.querySelector(".bc-media__play")).toBeNull();
  expect(video.controls).toBe(true);

  media(ws, "file", "m-9", "route.gpx", "application/gpx+xml", 18 * 1024);
  const file = [...root.querySelectorAll(".bc-card")].at(-1)!;
  expect(file.querySelector(".bc-option-title")?.textContent).toBe("route.gpx");
  expect(file.querySelector(".bc-caption")?.textContent).toBe(
    "application/gpx+xml · 18 KB · 已接收",
  );
  expect(file.querySelector("a[download]")?.getAttribute("href")).toBe(
    "/api/web/media/m-9",
  );

  // a picture whose URL no longer answers becomes a card that says so
  media(ws, "image", "m-10", "gone.jpg", "image/jpeg");
  const gone = [...root.querySelectorAll<HTMLElement>("button.bc-media")].at(
    -1,
  )!;
  gone.querySelector("img")!.dispatchEvent(new browser.window.Event("error"));
  const lost = [...root.querySelectorAll(".bc-card")].at(-1)!;
  expect(lost.querySelector(".bc-option-icon svg")).not.toBeNull();
  expect(lost.querySelector(".bc-option-title.bc-muted")?.textContent).toBe(
    "gone.jpg",
  );
  const unavailable = lost.querySelector(".bc-term")!;
  expect(unavailable.firstChild?.textContent).toBe("已不可用");
  expect(unavailable.querySelector(".bc-tooltip")?.textContent).toBe(
    "设备重启时清空了缓存中的附件",
  );
  expect(gone.isConnected).toBe(false);
});

test("the Viewer shows one picture at a time on the dark ground, pages, and gives focus back", () => {
  const { root, ws, button } = open();
  for (const index of [1, 2, 3, 4, 5])
    media(ws, "image", `m-${index}`, `${index}.png`, "image/png");
  const opener = root.querySelector<HTMLButtonElement>(
    ".bc-media-grid > .bc-media",
  )!;
  opener.focus();
  opener.click();
  const viewer = browser.document.querySelector<HTMLElement>(".bc-viewer")!;
  expect(viewer.parentElement).toBe(browser.document.body);
  expect(viewer.getAttribute("data-theme")).toBe("dark");
  expect(viewer.getAttribute("role")).toBe("dialog");
  expect(viewer.getAttribute("aria-modal")).toBe("true");
  expect(viewer.getAttribute("aria-label")).toBe("1.png");
  const bar = viewer.querySelector(".bc-viewer__bar")!;
  expect(bar.querySelector(".bc-small")?.textContent).toBe("1.png");
  const captions = [...bar.querySelectorAll(".bc-caption.bc-mono")].map(
    (node) => node.textContent,
  );
  expect(captions).toEqual(["PNG · 2 KB", "01 / 05"]);
  const close = button("关闭", viewer);
  expect(browser.document.activeElement).toBe(close);
  expect(button("下载", viewer).getAttribute("href")).toBe(
    "/api/web/media/m-1",
  );
  const image = viewer.querySelector<HTMLImageElement>(
    ".bc-viewer__stage > .bc-viewer__image > img",
  )!;
  expect(image.getAttribute("src")).toBe("/api/web/media/m-1");

  const key = (name: string) =>
    viewer.dispatchEvent(
      new browser.window.KeyboardEvent("keydown", { key: name }),
    );
  key("ArrowRight");
  expect(image.getAttribute("src")).toBe("/api/web/media/m-2");
  expect(viewer.textContent).toContain("02 / 05");
  const next = button("下一张", viewer);
  expect(next.className).toBe(
    "bc-button bc-button--outline bc-button--icon bc-viewer__nav",
  );
  next.click();
  expect(viewer.textContent).toContain("03 / 05");
  button("上一张", viewer).click();
  key("ArrowLeft");
  key("ArrowLeft");
  expect(viewer.textContent).toContain("05 / 05");
  key("Escape");
  expect(browser.document.querySelector(".bc-viewer")).toBeNull();
  expect(browser.document.activeElement).toBe(opener);

  // a click beside the picture closes too; one picture has no paging
  media(ws, "file", "m-6", "a.txt", "text/plain");
  media(ws, "image", "m-7", "solo.png", "image/png");
  const solo = [...root.querySelectorAll<HTMLElement>("button.bc-media")].at(
    -1,
  )!;
  solo.click();
  const alone = browser.document.querySelector<HTMLElement>(".bc-viewer")!;
  expect(alone.querySelector(".bc-viewer__nav")).toBeNull();
  expect(alone.textContent).not.toContain("/ 0");
  alone
    .querySelector<HTMLElement>(".bc-viewer__stage")!
    .dispatchEvent(new browser.window.Event("click", { bubbles: true }));
  expect(browser.document.querySelector(".bc-viewer")).toBeNull();
  expect(browser.document.activeElement).toBe(solo);
});
