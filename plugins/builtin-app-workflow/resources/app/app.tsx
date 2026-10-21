import { Show } from "solid-js";
import { For, Text, View } from "@pocketjs/framework/solid/components";
import { clock, empty, flows, toggle } from "./app";

// The loaded Workflows, what starts each, and a switch for each: a column
// under the clock, as wide as the display allows up to the 280 px the design
// draws it at, centered so it keeps to the middle a round display shows.
export default function Workflows() {
  return (
    <View class="w-full h-full flex-col items-center px-[16px] overflow-hidden bg-[#09090B]">
      <Text class="shrink-0 text-sm font-bold text-[#FAFAFA] mt-[16px]">{clock()}</Text>
      <View class="flex-1 min-h-0 w-full max-w-[280px] flex-col mt-[16px] overflow-hidden">
        <For each={flows()} by={(flow) => flow.id}>
          {(flow) => (
            <View class="shrink-0 w-full flex-col">
              <View class="w-full min-h-[73px] flex-row flex-wrap items-center gap-3 px-3 py-[8px]">
                <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{flow().glyph}</Text>
                <View class="basis-[72px] grow min-w-0 flex-col">
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
        <Show when={empty()}>
          <View class="w-full flex-1 flex-col items-center justify-center gap-2">
            <Text class="text-lg text-[#FAFAFA] text-center">No workflows yet</Text>
            <Text class="text-sm text-[#A1A1AA] text-center">Ask the Agent to set one up</Text>
          </View>
        </Show>
      </View>
    </View>
  );
}
