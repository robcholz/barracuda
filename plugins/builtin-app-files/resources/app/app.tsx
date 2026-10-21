import { Show } from "solid-js";
import { For, Text, View } from "@pocketjs/framework/solid/components";
import { area, areas, clock, empty, files, open, page, summary } from "./app";

// The Workspace's shared areas, and the files in one.
export default function Files() {
  return (
    <View class="w-full h-full relative overflow-hidden bg-[#09090B]">
      <View class="absolute left-0 right-0 top-[16px] flex-row justify-center">
        <Text class="text-sm font-bold text-[#FAFAFA]">{clock()}</Text>
      </View>

      <Show when={page() === 0}>
        <View class="absolute left-[40px] right-[40px] top-[52px] flex-col">
          <For each={areas()} by={(item) => item.id}>
            {(item) => (
              <View class="w-full h-[58px] flex-col" focusable onPress={() => open(item().id)}>
                <View class="w-full h-[57px] flex-row items-center gap-3 px-3">
                  <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{item().glyph}</Text>
                  <Text class="flex-1 text-lg text-[#FAFAFA]">{item().name}</Text>
                  <Text class="shrink-0 font-mono text-sm text-[#A1A1AA]">{item().count}</Text>
                </View>
                <View class="w-full h-[1px] bg-[#27272A]" />
              </View>
            )}
          </For>
        </View>
      </Show>

      <Show when={page() === 1}>
        <View class="absolute left-[40px] right-[40px] top-[52px] flex-col">
          <View class="w-full flex-row items-end justify-between px-3 pb-[8px]">
            <Text class="text-lg font-bold text-[#FAFAFA]">{area()}</Text>
            <Text class="text-sm text-[#A1A1AA]">{summary()}</Text>
          </View>
          <View class="w-full h-[1px] bg-[#27272A]" />
          <For each={files()} by={(file) => file.id}>
            {(file) => (
              <View class="w-full h-[46px] flex-col">
                <View class="w-full h-[45px] flex-row items-center gap-3 px-3">
                  <View class="flex-1 h-[20px] overflow-hidden">
                    <Text class="font-mono text-sm text-[#FAFAFA]">{file().path}</Text>
                  </View>
                  <Text class="shrink-0 font-mono text-sm text-[#A1A1AA]">{file().size}</Text>
                </View>
                <View class="w-full h-[1px] bg-[#27272A]" />
              </View>
            )}
          </For>
        </View>
        <Show when={empty()}>
          <View class="absolute left-0 right-0 top-[150px] flex-row justify-center">
            <Text class="text-sm text-[#A1A1AA]">No files yet</Text>
          </View>
        </Show>
      </Show>
    </View>
  );
}
