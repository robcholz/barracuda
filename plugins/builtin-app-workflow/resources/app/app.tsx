import { Show } from "solid-js";
import { For, Text, View } from "@pocketjs/framework/solid/components";
import { clock, empty, flows, toggle } from "./app";

// The loaded Workflows, what starts each, and a switch for each.
export default function Workflows() {
  return (
    <View class="w-full h-full relative overflow-hidden bg-[#09090B]">
      <View class="absolute left-0 right-0 top-[16px] flex-row justify-center">
        <Text class="text-sm font-bold text-[#FAFAFA]">{clock()}</Text>
      </View>
      <View class="absolute left-[40px] right-[40px] top-[52px] flex-col">
        <For each={flows()} by={(flow) => flow.id}>
          {(flow) => (
            <View class="w-full h-[74px] flex-col">
              <View class="w-full h-[73px] flex-row items-center gap-3 px-3">
                <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{flow().glyph}</Text>
                <View class="flex-1 flex-col overflow-hidden">
                  <Text class="text-lg text-[#FAFAFA]">{flow().name}</Text>
                  <Text class="text-sm text-[#A1A1AA]">{flow().detail}</Text>
                </View>
                <View
                  focusable
                  onPress={() => toggle(flow().id)}
                  class={
                    flow().on
                      ? "shrink-0 w-[46px] h-[28px] rounded-full bg-[#FAFAFA] flex-row items-center justify-end px-[3px]"
                      : "shrink-0 w-[46px] h-[28px] rounded-full bg-[#27272A] border border-[#3F3F46] flex-row items-center justify-start px-[3px]"
                  }
                >
                  <View class={flow().on ? "w-[22px] h-[22px] rounded-full bg-[#09090B]" : "w-[20px] h-[20px] rounded-full bg-[#09090B]"} />
                </View>
              </View>
              <View class="w-full h-[1px] bg-[#27272A]" />
            </View>
          )}
        </For>
      </View>
      <Show when={empty()}>
        <View class="absolute left-0 top-0 w-full h-full flex-col items-center justify-center gap-2">
          <Text class="text-lg text-[#FAFAFA]">No workflows yet</Text>
          <Text class="text-sm text-[#A1A1AA]">Ask the Agent to set one up</Text>
        </View>
      </Show>
    </View>
  );
}
