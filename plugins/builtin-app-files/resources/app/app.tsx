import { Show } from "solid-js";
import { For, Text, View } from "@pocketjs/framework/solid/components";
import { area, areas, clock, empty, files, open, page, summary } from "./app";

// The Workspace's shared areas, and the files in one: a column under the
// clock, as wide as the display allows up to the 280 px the design draws it
// at, centered so it keeps to the middle a round display shows.
export default function Files() {
  return (
    <View class="w-full h-full flex-col items-center px-[16px] overflow-hidden bg-[#09090B]">
      <Text class="shrink-0 text-sm font-bold text-[#FAFAFA] mt-[16px]">{clock()}</Text>

      <Show when={page() === 0}>
        <View class="flex-1 min-h-0 w-full max-w-[280px] flex-col mt-[16px] overflow-hidden">
          <For each={areas()} by={(item) => item.id}>
            {(item) => (
              <View class="shrink-0 w-full flex-col" focusable onPress={() => open(item().id)}>
                <View class="w-full min-h-[57px] flex-row flex-wrap items-center gap-3 px-3 py-[6px]">
                  <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{item().glyph}</Text>
                  <Text class="basis-[72px] grow min-w-0 text-lg text-[#FAFAFA]">{item().name}</Text>
                  <Text class="shrink-0 font-mono text-sm text-[#A1A1AA]">{item().count}</Text>
                </View>
                <View class="w-full h-[1px] bg-[#27272A]" />
              </View>
            )}
          </For>
        </View>
      </Show>

      <Show when={page() === 1}>
        <View class="flex-1 min-h-0 w-full max-w-[280px] flex-col mt-[16px] overflow-hidden">
          <View class="shrink-0 w-full flex-row flex-wrap items-end justify-between gap-2 px-3 pb-[8px]">
            <Text class="shrink-0 text-lg font-bold text-[#FAFAFA]">{area()}</Text>
            <Text class="min-w-0 text-sm text-[#A1A1AA]">{summary()}</Text>
          </View>
          <View class="shrink-0 w-full h-[1px] bg-[#27272A]" />
          <For each={files()} by={(file) => file.id}>
            {(file) => (
              <View class="shrink-0 w-full flex-col">
                <View class="w-full h-[45px] flex-row items-center gap-3 px-3">
                  <View class="flex-1 min-w-0 h-[20px] overflow-hidden">
                    <Text class="font-mono text-sm text-[#FAFAFA]">{file().path}</Text>
                  </View>
                  <Text class="shrink-0 font-mono text-sm text-[#A1A1AA]">{file().size}</Text>
                </View>
                <View class="w-full h-[1px] bg-[#27272A]" />
              </View>
            )}
          </For>
          <Show when={empty()}>
            <View class="w-full flex-1 items-center justify-center">
              <Text class="text-sm text-[#A1A1AA]">No files yet</Text>
            </View>
          </Show>
        </View>
      </Show>
    </View>
  );
}
