/**
 * The agent on Anthropic's Messages API, through the official SDK, from the browser with the
 * person's own key. Responses stream; thinking is adaptive with summaries the chat can show;
 * tool inputs stream as they are generated and are checked here before they run.
 */

import Anthropic from "@anthropic-ai/sdk";
import type { Attachment } from "../state/project";
import type { Service, StepEvents, StepResult } from "./service";
import type { ToolSpec } from "./tools";

/** Models whose refusals the API can retry on another model (`fallbacks: "default"`). */
const FALLBACK_MODELS = new Set([
  "claude-fable-5-1",
  "claude-opus-5-5",
  "claude-opus-5",
  "claude-sonnet-5-5",
]);

function userContent(
  text: string,
  attachments: readonly Attachment[],
): Anthropic.Beta.BetaContentBlockParam[] {
  const content: Anthropic.Beta.BetaContentBlockParam[] = [];
  for (const attachment of attachments) {
    if (attachment.data && /^image\/(png|jpeg|gif|webp)$/.test(attachment.type))
      content.push({
        type: "image",
        source: {
          type: "base64",
          media_type: attachment.type as "image/png",
          data: attachment.data,
        },
      });
    else if (attachment.text !== undefined)
      content.push({
        type: "text",
        text: `Attached file ${attachment.name}:\n\n${attachment.text}`,
      });
  }
  content.push({ type: "text", text });
  return content;
}

export function anthropic(apiKey: string, model: string): Service {
  const client = new Anthropic({ apiKey, dangerouslyAllowBrowser: true });
  async function once(
    system: string,
    tools: ToolSpec[],
    messages: unknown[],
    signal: AbortSignal,
    events: StepEvents,
  ): Promise<StepResult> {
    const fallback = FALLBACK_MODELS.has(model);
    const stream = client.beta.messages.stream(
      {
        model,
        max_tokens: 64000,
        system: [{ type: "text", text: system }],
        tools: tools.map((tool) => ({
          name: tool.name,
          description: tool.description,
          input_schema: tool.parameters,
          eager_input_streaming: true,
        })),
        messages: messages as Anthropic.Beta.BetaMessageParam[],
        thinking: { type: "adaptive", display: "summarized" },
        output_config: { effort: "high" },
        cache_control: { type: "ephemeral" },
        ...(fallback
          ? {
              fallbacks: "default" as const,
              betas: ["server-side-fallback-2026-07-01"],
            }
          : {}),
      },
      { signal },
    );
    for await (const event of stream) {
      if (
        event.type === "content_block_start" &&
        event.content_block.type === "tool_use"
      )
        events.tool(event.content_block.id, event.content_block.name);
      if (event.type === "content_block_delta") {
        if (event.delta.type === "thinking_delta")
          events.thinking(event.delta.thinking);
        if (event.delta.type === "text_delta") events.text(event.delta.text);
      }
    }
    const message = await stream.finalMessage();
    const calls = message.content
      .filter(
        (block): block is Anthropic.Beta.BetaToolUseBlock =>
          block.type === "tool_use",
      )
      .map((block) => ({ id: block.id, name: block.name, input: block.input }));
    const text = message.content
      .filter(
        (block): block is Anthropic.Beta.BetaTextBlock => block.type === "text",
      )
      .map((block) => block.text)
      .join("");
    const thinking = message.content
      .filter(
        (block): block is Anthropic.Beta.BetaThinkingBlock =>
          block.type === "thinking",
      )
      .map((block) => block.thinking)
      .join("\n\n");
    // The whole content goes back unchanged, thinking blocks included, as the API requires.
    const assistant = { role: "assistant", content: message.content };
    if (message.stop_reason === "refusal")
      return {
        message: assistant,
        text,
        thinking,
        calls: [],
        stop: "refused",
        detail: message.stop_details?.explanation ?? undefined,
      };
    // A tool input cut off at max_tokens may still parse; never run it.
    if (message.stop_reason === "max_tokens")
      return {
        message: assistant,
        text,
        thinking,
        calls: [],
        stop: "truncated",
      };
    return {
      message: assistant,
      text,
      thinking,
      calls,
      stop: calls.length ? "tools" : "end",
    };
  }

  return {
    id: "anthropic",
    user: (text, attachments) => ({
      role: "user",
      content: userContent(text, attachments),
    }),
    results: (results) => [
      {
        role: "user",
        content: results.map(({ id, output }) => ({
          type: "tool_result",
          tool_use_id: id,
          is_error: output.isError || undefined,
          content: [
            { type: "text", text: output.text },
            ...(output.image
              ? [
                  {
                    type: "image",
                    source: {
                      type: "base64",
                      media_type: "image/png",
                      data: output.image,
                    },
                  },
                ]
              : []),
          ],
        })),
      },
    ],

    async step(system, tools, messages, signal, events) {
      // With eager input streaming a tool input may not parse at all; only that is tried again.
      for (let attempt = 0; ; attempt += 1) {
        try {
          return await once(system, tools, messages, signal, events);
        } catch (error) {
          if (
            error instanceof Anthropic.APIError ||
            signal.aborted ||
            attempt >= 2
          )
            throw error;
        }
      }
    },

    describe(error) {
      if (error instanceof Anthropic.AuthenticationError)
        return "Anthropic did not accept the API key. Check it in Settings.";
      if (error instanceof Anthropic.PermissionDeniedError)
        return "This API key may not use the model. Check the model in Settings.";
      if (error instanceof Anthropic.NotFoundError)
        return `Anthropic does not know the model ${model}. Check it in Settings.`;
      if (error instanceof Anthropic.RateLimitError)
        return "Anthropic is rate limiting this key. Wait a moment and send again.";
      if (error instanceof Anthropic.BadRequestError)
        return `Anthropic refused the request: ${error.message}`;
      if (error instanceof Anthropic.APIConnectionError)
        return "Could not reach Anthropic. Check the connection.";
      if (error instanceof Anthropic.APIError)
        return `Anthropic failed (${error.status}): ${error.message}`;
      return error instanceof Error ? error.message : String(error);
    },
  };
}
