/**
 * The agent on a service with the OpenAI Chat Completions API, at the base URL the person gives:
 * streamed responses with function calls, over `fetch`. The service must allow requests from this
 * page's origin (CORS).
 */

import type { Attachment } from "../state/project";
import type { Service, StepResult } from "./service";
import { t } from "../i18n";

interface ChatMessage {
  role: "system" | "user" | "assistant" | "tool";
  content:
    | string
    | null
    | { type: string; text?: string; image_url?: { url: string } }[];
  tool_calls?: {
    id: string;
    type: "function";
    function: { name: string; arguments: string };
  }[];
  tool_call_id?: string;
}

/** A service's answer that is not a stream: its error. */
export class ServiceError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

/** Reads server-sent events from a response body: each `data:` payload. */
export async function* events(
  body: ReadableStream<Uint8Array>,
): AsyncGenerator<string> {
  const reader = body
    .pipeThrough(
      new TextDecoderStream() as unknown as TransformStream<Uint8Array, string>,
    )
    .getReader();
  let buffer = "";
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += value;
    let end: number;
    while ((end = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, end).replace(/\r$/, "");
      buffer = buffer.slice(end + 1);
      if (line.startsWith("data:")) yield line.slice(5).trim();
    }
  }
}

export function openai(
  baseUrl: string,
  apiKey: string,
  model: string,
): Service {
  const url = `${baseUrl.replace(/\/+$/, "")}/chat/completions`;
  return {
    id: "openai",
    user(text: string, attachments: readonly Attachment[]): ChatMessage {
      const parts: NonNullable<Exclude<ChatMessage["content"], string | null>> =
        [];
      for (const attachment of attachments) {
        if (attachment.data)
          parts.push({
            type: "image_url",
            image_url: {
              url: `data:${attachment.type};base64,${attachment.data}`,
            },
          });
        else if (attachment.text !== undefined)
          parts.push({
            type: "text",
            text: `Attached file ${attachment.name}:\n\n${attachment.text}`,
          });
      }
      return parts.length
        ? { role: "user", content: [...parts, { type: "text", text }] }
        : { role: "user", content: text };
    },
    results(results) {
      const messages: ChatMessage[] = results.map(({ id, output }) => ({
        role: "tool",
        tool_call_id: id,
        content: output.isError ? `Error: ${output.text}` : output.text,
      }));
      // Tool messages carry text only; a screenshot follows as a user message.
      const images = results.filter(({ output }) => output.image);
      if (images.length)
        messages.push({
          role: "user",
          content: images.map(({ output }) => ({
            type: "image_url",
            image_url: { url: `data:image/png;base64,${output.image}` },
          })),
        });
      return messages;
    },

    async step(system, tools, messages, signal, on): Promise<StepResult> {
      const reply = await fetch(url, {
        method: "POST",
        signal,
        headers: {
          "content-type": "application/json",
          ...(apiKey ? { authorization: `Bearer ${apiKey}` } : {}),
        },
        body: JSON.stringify({
          model,
          stream: true,
          messages: [
            { role: "system", content: system },
            ...(messages as ChatMessage[]),
          ],
          tools: tools.map((tool) => ({
            type: "function",
            function: {
              name: tool.name,
              description: tool.description,
              parameters: tool.parameters,
            },
          })),
        }),
      });
      if (!reply.ok || !reply.body) {
        let message = reply.statusText;
        try {
          const body = (await reply.json()) as { error?: { message?: string } };
          message = body.error?.message ?? message;
        } catch {
          // the status says enough
        }
        throw new ServiceError(reply.status, message);
      }
      let text = "";
      let thinking = "";
      let finish = "";
      const calls: { id: string; name: string; arguments: string }[] = [];
      for await (const data of events(reply.body)) {
        if (data === "[DONE]") break;
        const chunk = JSON.parse(data) as {
          choices?: {
            delta?: {
              content?: string | null;
              reasoning_content?: string | null;
              tool_calls?: {
                index: number;
                id?: string;
                function?: { name?: string; arguments?: string };
              }[];
            };
            finish_reason?: string | null;
          }[];
        };
        const choice = chunk.choices?.[0];
        if (!choice) continue;
        const delta = choice.delta ?? {};
        if (delta.reasoning_content) {
          thinking += delta.reasoning_content;
          on.thinking(delta.reasoning_content);
        }
        if (delta.content) {
          text += delta.content;
          on.text(delta.content);
        }
        for (const call of delta.tool_calls ?? []) {
          const slot = (calls[call.index] ??= {
            id: "",
            name: "",
            arguments: "",
          });
          if (call.id) slot.id = call.id;
          if (call.function?.name) {
            slot.name += call.function.name;
            on.tool(slot.id || `call-${call.index}`, slot.name);
          }
          if (call.function?.arguments)
            slot.arguments += call.function.arguments;
        }
        if (choice.finish_reason) finish = choice.finish_reason;
      }
      const made = calls
        .filter(Boolean)
        .map((call, index) => ({ ...call, id: call.id || `call-${index}` }));
      const message: ChatMessage = {
        role: "assistant",
        content: text || null,
        ...(made.length
          ? {
              tool_calls: made.map((call) => ({
                id: call.id,
                type: "function" as const,
                function: { name: call.name, arguments: call.arguments },
              })),
            }
          : {}),
      };
      if (finish === "length")
        return { message, text, thinking, calls: [], stop: "truncated" };
      if (finish === "content_filter")
        return { message, text, thinking, calls: [], stop: "refused" };
      const parsed = made.map((call) => {
        let input: unknown;
        try {
          input = JSON.parse(call.arguments || "{}");
        } catch {
          input = { INVALID_JSON: call.arguments };
        }
        return { id: call.id, name: call.name, input };
      });
      return {
        message,
        text,
        thinking,
        calls: parsed,
        stop: parsed.length ? "tools" : "end",
      };
    },

    describe(error) {
      if (error instanceof ServiceError) {
        if (error.status === 401 || error.status === 403) return t.openai.auth;
        if (error.status === 404) return t.openai.notFound(url, model);
        if (error.status === 429) return t.openai.rateLimit;
        return t.openai.failed(error.status, error.message);
      }
      if (error instanceof TypeError) return t.openai.connection;
      return error instanceof Error ? error.message : String(error);
    },
  };
}
