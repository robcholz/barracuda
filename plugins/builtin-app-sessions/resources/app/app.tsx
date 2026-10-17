import { Show } from "solid-js";
import { For, Text, View } from "@pocketjs/framework/solid/components";
import { clock, empty, messages, open, page, sessions, thinking, title } from "./app";

export default function Sessions() {
  return (
    <View class="w-full h-full relative overflow-hidden bg-[#09090B]">
      <View class="absolute left-0 right-0 top-[16px] flex-row justify-center">
        <Text class="text-sm font-bold text-[#FAFAFA]">{clock()}</Text>
      </View>

      <Show when={page() === 0}>
        <View class="absolute left-[40px] right-[40px] top-[52px] flex-col">
          <For each={sessions()} by={(session) => session.id}>
            {(session) => (
              <View class="w-full h-[74px] flex-col" focusable onPress={() => open(session().id)}>
                <View class="w-full h-[73px] flex-row items-center gap-3 px-3">
                  <View class="w-2 h-2" />
                  <View class="flex-1 flex-col overflow-hidden">
                    <Text class="text-base text-[#FAFAFA]">{session().title}</Text>
                    <Text class="text-base text-[#A1A1AA]">{session().detail}</Text>
                  </View>
                  <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{""}</Text>
                </View>
                <View class="w-full h-[1px] bg-[#27272A]" />
              </View>
            )}
          </For>
        </View>
        <Show when={empty()}>
          <View class="absolute left-0 top-0 w-full h-full flex-col items-center justify-center gap-2">
            <Text class="text-lg text-[#FAFAFA]">还没有会话</Text>
            <Text class="text-sm text-[#A1A1AA]">和 Agent 说话后，对话会出现在这里</Text>
          </View>
        </Show>
      </Show>

      <Show when={page() === 1}>
        {/* The latest messages, the newest at the bottom; older ones go off the top. */}
        <View class="absolute left-[40px] right-[40px] top-[44px] bottom-[40px] flex-col justify-end gap-[12px] overflow-hidden">
          <For each={messages()} by={(message) => message.id}>
            {(message) => (
              <View class={message().mine ? "w-full flex-row justify-end" : "w-full flex-col gap-[6px]"}>
                <Show when={message().mine}>
                  <View class="max-w-[220px] px-[14px] py-[8px] rounded-lg bg-[#18181B] border border-[#27272A]">
                    <Text class="text-base text-[#FAFAFA]">{message().text}</Text>
                  </View>
                </Show>
                <Show when={!message().mine}>
                  <View class="w-[24px] h-[24px] rounded-[6px] bg-[#232327] items-center justify-center">
                    <Text class="text-sm font-bold text-[#CCFF00]">{""}</Text>
                  </View>
                  <Text class="w-full text-base text-[#FAFAFA]">{message().text}</Text>
                </Show>
              </View>
            )}
          </For>
          <Show when={thinking()}>
            <Text class="text-sm text-[#A1A1AA] animate-spin">{""}</Text>
          </Show>
        </View>
        <View class="absolute left-[60px] right-[60px] bottom-[14px] flex-row justify-center overflow-hidden">
          <Text class="text-base text-[#A1A1AA]">{title()}</Text>
        </View>
      </Show>
    </View>
  );
}
