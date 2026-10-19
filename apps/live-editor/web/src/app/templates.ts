/**
 * What a new app starts from: a blank app with a view, or one of the app manager's examples
 * (plugins/app-manager/resources/examples), which build.ts copies to `templates.json`.
 */

import type { Files } from "../state/project";

export interface Template {
  id: string;
  title: string;
  text: string;
  files: Files;
}

/** The examples New app offers, in the order it lists them, with what each shows. */
export const EXAMPLES = [
  { id: "counter", title: "Counter", text: "Buttons, a list, saved state" },
  { id: "air", title: "Air quality", text: "Readings from a sensor" },
  {
    id: "lamp",
    title: "Light strip",
    text: "A color picker for a light strip",
  },
  { id: "camera", title: "Camera", text: "Take and show a picture" },
] as const;

const POCKET = (title: string) =>
  `${JSON.stringify(
    {
      pocket: 2,
      id: "dev.barracuda.app",
      name: "barracuda-app",
      title,
      version: "0.1.0",
      engine: {
        capabilities: { requires: ["text.glyphs.baked", "input.buttons"] },
      },
      app: {
        entry: "app.tsx",
        framework: "solid",
        viewport: {
          fixed: { logical: [360, 360], presentation: "integer-fit" },
        },
        aot: true,
      },
    },
    null,
    2,
  )}\n`;

/** A blank app: one line of text the program sets, on the round display. */
export const BLANK: Files = {
  "app.json": `${JSON.stringify(
    {
      format: 1,
      id: "app",
      name: "New app",
      version: "0.1.0",
      api: 1,
      view: { shape: "round" },
    },
    null,
    2,
  )}\n`,
  "pocket.json": POCKET("New app"),
  "app.d.ts": `// The contract between the view and the app's Lua code: values Lua writes
// with \`view:set\`, and handlers Lua receives as events.
import type { Accessor } from "solid-js";
export declare const message: Accessor<string>;
`,
  "app.tsx": `import { Text, View } from "@pocketjs/framework/solid/components";
import { message } from "./app";

export default function App() {
  return (
    <View class="w-full h-full flex-col items-center justify-center bg-[#09090B]">
      <Text class="text-xl text-[#FAFAFA]">{message()}</Text>
    </View>
  );
}
`,
  "main.lua": `local app = require("app")
local view = assert(app.view())

assert(view:set("message", "Hello"))
for event in app.events() do
  if event == "app:terminate" then break end
end
`,
};

let loaded: Promise<Record<string, Files>> | undefined;

/** The examples' sources, by example ID. */
export function examples(base: URL): Promise<Record<string, Files>> {
  loaded ??= fetch(new URL("templates.json", base)).then(
    (reply) => reply.json() as Promise<Record<string, Files>>,
  );
  return loaded;
}
