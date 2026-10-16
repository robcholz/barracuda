import { Show } from "solid-js";
import { For, Text, View } from "@pocketjs/framework/solid/components";
import { count, history, increment, reset } from "./app";

export default function App() {
  return (
    <View class="w-full h-full flex-col items-center justify-center gap-3 bg-[#09090B]">
      <Text class="text-2xl font-bold text-[#FAFAFA]">{count()}</Text>
      <View class="flex-row gap-2">
        <View class="px-4 py-2 rounded-md bg-[#CCFF00] focus:bg-[#BAE634]" focusable onPress={() => increment(1)}>
          <Text class="text-[#09090B]">+1</Text>
        </View>
        <View class="px-4 py-2 rounded-md bg-[#27272A] focus:bg-[#3F3F46]" focusable onPress={() => reset()}>
          <Text class="text-[#FAFAFA]">Reset</Text>
        </View>
      </View>
      <Show when={count() > 0}>
        <For each={history()} by={(entry) => entry.id}>
          {(entry) => <Text class="text-sm text-[#A1A1AA]">{entry().label}</Text>}
        </For>
      </Show>
    </View>
  );
}
