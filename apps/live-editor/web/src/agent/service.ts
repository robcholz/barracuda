/**
 * A model service the agent talks to: Anthropic's Messages API, or a service with the OpenAI
 * Chat Completions API. Each keeps its own message format; the agent stores what it sent and
 * received in that format, so a conversation goes back to the service exactly as it was.
 */

import type { Attachment } from "../state/project";
import type { ToolOutput, ToolSpec } from "./tools";

/** What the agent shows while a response streams. */
export interface StepEvents {
  thinking(delta: string): void;
  text(delta: string): void;
  tool(id: string, name: string): void;
}

/** One model response, read back. */
export interface StepResult {
  /** The assistant message to append, in the service's own format. */
  message: unknown;
  text: string;
  thinking: string;
  calls: { id: string; name: string; input: unknown }[];
  /** Why the response ended: it finished, wants tools, or was cut short. */
  stop: "end" | "tools" | "refused" | "truncated";
  /** What the service said about a refusal. */
  detail?: string;
}

export interface Service {
  readonly id: "anthropic" | "openai";
  /** A user message with its attachments. */
  user(text: string, attachments: readonly Attachment[]): unknown;
  /** The messages that answer `calls`, in order. */
  results(results: { id: string; output: ToolOutput }[]): unknown[];
  step(
    system: string,
    tools: ToolSpec[],
    messages: unknown[],
    signal: AbortSignal,
    events: StepEvents,
  ): Promise<StepResult>;
  /** Words for a failure, for the chat. */
  describe(error: unknown): string;
}
