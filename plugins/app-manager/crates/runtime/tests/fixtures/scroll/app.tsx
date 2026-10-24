import { For, Text, View } from "@pocketjs/framework/solid/components";
import { end, pick, rows } from "./app";

// A title over a list that takes the rest of the display and scrolls when
// its rows run past it; with `end`, the rows sit at its bottom, as a chat's.
export default function App() {
  return (
    <View class="w-full h-full flex-col bg-[#09090B]">
      <Text class="shrink-0 h-[40px] text-[#FAFAFA]">Rows</Text>
      <View class={end() ? "flex-1 min-h-0 w-full flex-col justify-end overflow-y-auto" : "flex-1 min-h-0 w-full flex-col overflow-y-auto"}>
        <For each={rows()} by={(row) => row.id}>
          {(row) => (
            <View class="shrink-0 w-full h-[40px] bg-[#18181B]" focusable onPress={() => pick(row().id)}>
              <Text class="text-[#FAFAFA]">{row().name}</Text>
            </View>
          )}
        </For>
      </View>
    </View>
  );
}
