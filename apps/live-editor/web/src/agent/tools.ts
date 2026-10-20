/**
 * The agent's tools, described once for every model service, and what each does to the open app:
 * file edits go into the proposal the person reviews; build, look, tap and the console work on
 * the preview.
 */

/** A tool as both model services describe it: a name, what it does, and a JSON schema. */
export interface ToolSpec {
  name: string;
  description: string;
  parameters: {
    type: "object";
    properties: Record<
      string,
      { type: "string" | "integer" | "number"; description: string }
    >;
    required: string[];
    additionalProperties: false;
  };
}

const spec = (
  name: string,
  description: string,
  properties: ToolSpec["parameters"]["properties"] = {},
  required: string[] = Object.keys(properties),
): ToolSpec => ({
  name,
  description,
  parameters: {
    type: "object",
    properties,
    required,
    additionalProperties: false,
  },
});

export const TOOLS: ToolSpec[] = [
  spec("list_files", "Lists the app's source files with their sizes in lines."),
  spec("read_file", "Reads one source file, with line numbers.", {
    path: { type: "string", description: "The file name, such as app.tsx" },
  }),
  spec(
    "write_file",
    "Writes a whole source file, creating it or replacing it. The change is proposed to the person.",
    {
      path: {
        type: "string",
        description:
          "The file name: letters, digits, dots, dashes, underscores",
      },
      content: { type: "string", description: "The file's complete new text" },
    },
  ),
  spec(
    "edit_file",
    "Replaces one exact, unique piece of a source file. Include enough surrounding text to make old_text unique.",
    {
      path: { type: "string", description: "The file name" },
      old_text: {
        type: "string",
        description: "The exact text to replace; it must occur exactly once",
      },
      new_text: { type: "string", description: "Its replacement" },
    },
  ),
  spec("delete_file", "Deletes a source file the app no longer needs.", {
    path: { type: "string", description: "The file name" },
  }),
  spec(
    "build",
    "Builds the app with the proposed changes, as the toolchain builds it for a device, and restarts it in the preview. Answers the first problem with its file and line, or that it built.",
  ),
  spec(
    "look",
    "Shows the preview's screen as an image, with the text on it, the view's values and handlers, and whether the app runs.",
  ),
  spec(
    "tap",
    "Taps the preview's screen at a point, then shows the screen as look does.",
    {
      x: { type: "integer", description: "Pixels from the left of the view" },
      y: { type: "integer", description: "Pixels from the top of the view" },
    },
  ),
  spec(
    "read_console",
    "Reads the preview's latest console lines: the program's print output, errors and lifecycle events.",
  ),
  spec(
    "restart",
    "Restarts the app in the preview from its stored state, as opening it again on the device would.",
  ),
];

/** A tool's answer: text, an optional PNG, and whether the call failed. */
export interface ToolOutput {
  text: string;
  image?: string;
  isError?: boolean;
}

/** Checks a model's tool input against the tool's schema: the service may not have checked it. */
export function checkInput(name: string, input: unknown): string | null {
  const tool = TOOLS.find((item) => item.name === name);
  if (!tool) return `there is no tool named ${name}`;
  if (!input || typeof input !== "object" || Array.isArray(input))
    return "the input must be an object";
  const record = input as Record<string, unknown>;
  for (const key of tool.parameters.required)
    if (!(key in record)) return `${key} is missing`;
  for (const [key, value] of Object.entries(record)) {
    const property = tool.parameters.properties[key];
    if (!property) return `${key} is not a parameter of ${name}`;
    if (property.type === "string" && typeof value !== "string")
      return `${key} must be a string`;
    if (
      property.type === "integer" &&
      !(typeof value === "number" && Number.isInteger(value))
    )
      return `${key} must be an integer`;
    if (property.type === "number" && typeof value !== "number")
      return `${key} must be a number`;
  }
  return null;
}

/** Applies one edit_file: the text with `oldText` replaced, or why it cannot be. */
export function replaceOnce(
  text: string,
  oldText: string,
  newText: string,
): string | { error: string } {
  if (!oldText) return { error: "old_text is empty" };
  const first = text.indexOf(oldText);
  if (first < 0)
    return { error: "old_text does not occur in the file; read it again" };
  if (text.indexOf(oldText, first + 1) >= 0)
    return {
      error: "old_text occurs more than once; include more surrounding text",
    };
  return text.slice(0, first) + newText + text.slice(first + oldText.length);
}

/** A file with line numbers, as read_file answers it. */
export function numbered(text: string): string {
  const lines = text.split("\n");
  const width = String(lines.length).length;
  return lines
    .map((line, index) => `${String(index + 1).padStart(width)}\t${line}`)
    .join("\n");
}
