/** The page's side of the build and run workers. */

import type { AppStorage, Display, RunManifest } from "../run/app-run";
import type { Files } from "../state/project";
import type { BuildReply } from "../workers/build";
import type { RunReply, RunRequest } from "../workers/run";
import type { ViewContract } from "../wasm";

/** Builds sources in the build worker, one build at a time; a newer request supersedes waiting ones. */
export class Builder {
  private worker: Worker;
  private next = 0;
  private pending = new Map<number, (reply: BuildReply) => void>();

  constructor(url: URL) {
    this.worker = new Worker(url, { type: "module" });
    this.worker.onmessage = (event: MessageEvent<BuildReply>) => {
      const resolve = this.pending.get(event.data.id);
      this.pending.delete(event.data.id);
      resolve?.(event.data);
    };
    this.worker.onerror = (event) => {
      for (const [id, resolve] of this.pending)
        resolve({
          id,
          ok: false,
          problem: { message: `the build worker failed: ${event.message}` },
          ms: 0,
        });
      this.pending.clear();
    };
  }

  build(files: Files): Promise<BuildReply> {
    const id = ++this.next;
    return new Promise((resolve) => {
      this.pending.set(id, resolve);
      this.worker.postMessage({ id, files });
    });
  }
}

/** What the run worker reports, as the page listens to it. */
export type RunListener = (reply: RunReply) => void;

/** Runs one app at a time in the run worker. */
export class Runner {
  private worker: Worker;
  private inspections = new Map<
    number,
    (reply: {
      texts: string[];
      contract: ViewContract | null;
      state: string;
    }) => void
  >();
  private next = 0;

  constructor(
    private readonly url: URL,
    private readonly listen: RunListener,
  ) {
    this.worker = this.spawn();
  }

  private spawn() {
    const worker = new Worker(this.url, { type: "module" });
    worker.onmessage = (event: MessageEvent<RunReply>) => {
      const reply = event.data;
      if (reply.type === "inspected") {
        this.inspections.get(reply.id)?.(reply);
        this.inspections.delete(reply.id);
        return;
      }
      this.listen(reply);
    };
    worker.onerror = (event) => {
      this.listen({
        type: "log",
        entry: {
          level: "error",
          text: `the preview failed: ${event.message}`,
          source: "system",
          at: Date.now(),
        },
      });
      this.listen({
        type: "state",
        state: "ended",
        reason: "the preview failed",
      });
    };
    return worker;
  }

  private send(request: RunRequest) {
    this.worker.postMessage(request);
  }

  start(
    files: Record<string, Uint8Array>,
    manifest: RunManifest,
    display: Display | null,
    storage: AppStorage,
    scale: number,
  ) {
    this.send({ type: "start", files, manifest, display, storage, scale });
  }

  tap(x: number, y: number) {
    this.send({ type: "tap", x, y });
  }

  lifecycle(type: "back" | "background" | "foreground" | "terminate" | "stop") {
    this.send({ type });
  }

  answer(allowed: boolean) {
    this.send({ type: "answer", allowed });
  }

  /** The running view's text and contract, for the agent; the run worker answers within a second. */
  inspect(): Promise<{
    texts: string[];
    contract: ViewContract | null;
    state: string;
  }> {
    const id = ++this.next;
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        this.inspections.delete(id);
        resolve({ texts: [], contract: null, state: "unresponsive" });
      }, 1000);
      this.inspections.set(id, (reply) => {
        clearTimeout(timer);
        resolve(reply);
      });
      this.send({ type: "inspect", id });
    });
  }

  /** Replaces a worker that stopped answering. */
  restart() {
    this.worker.terminate();
    this.worker = this.spawn();
  }
}
