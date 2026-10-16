import { Show } from "solid-js";
import { For, Text, View } from "@pocketjs/framework/solid/components";
import { asked, clock, empty, open, page, said, sessions, thinking, when } from "./app";

export default function Sessions() {
  return (
    <View class="w-full h-full relative overflow-hidden bg-[#09090B]">
      <View class="absolute left-0 right-0 top-[16px] flex-row justify-center">
        <Text class="text-sm font-bold text-[#FAFAFA]">{clock()}</Text>
      </View>

      <Show when={page() === 0}>
        <View class="absolute left-[40px] top-[52px] w-[280px] flex-col">
          <For each={sessions()} by={(session) => session.id}>
            {(session) => (
              <View class="w-[280px] h-[74px] flex-col" focusable onPress={() => open(session().id)}>
                <View class="w-[280px] h-[73px] flex-row items-center gap-3 px-3">
                  <View class="w-2 h-2" />
                  <View class="w-[206px] flex-col overflow-hidden">
                    <Text class="text-base text-[#FAFAFA]">{session().title}</Text>
                    <Text class="text-base text-[#A1A1AA]">{session().detail}</Text>
                  </View>
                  <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{""}</Text>
                </View>
                <View class="w-[280px] h-[1px] bg-[#27272A]" />
              </View>
            )}
          </For>
        </View>
        <Show when={empty()}>
          <View class="absolute left-0 top-0 w-[360px] h-[360px] flex-col items-center justify-center gap-2">
            <Text class="text-lg text-[#FAFAFA]">还没有会话</Text>
            <Text class="text-sm text-[#A1A1AA]">和 Agent 说话后，对话会出现在这里</Text>
          </View>
        </Show>
      </Show>

      <Show when={page() === 1}>
        <View class="absolute left-[40px] top-[82px] w-[280px] flex-col">
          <View class="w-[280px] flex-row justify-end">
            <View class="max-w-[220px] px-[14px] py-[8px] rounded-lg bg-[#18181B] border border-[#27272A]">
              <Text class="text-base text-[#FAFAFA]">{asked()}</Text>
            </View>
          </View>
          <View class="mt-[14px] flex-row items-center gap-2">
            <View class="w-[24px] h-[24px] rounded-[6px] bg-[#232327] items-center justify-center">
              <Text class="text-sm font-bold text-[#CCFF00]">{""}</Text>
            </View>
            <Text class="text-sm text-[#A1A1AA]">{when()}</Text>
            <Show when={thinking()}>
              <Text class="text-sm text-[#A1A1AA] animate-spin">{""}</Text>
            </Show>
          </View>
          <Text class="mt-[8px] w-[280px] text-base text-[#FAFAFA]">{said()}</Text>
        </View>
      </Show>
    </View>
  );
}
