import { Show } from "solid-js";
import { For, Text, View } from "@pocketjs/framework/solid/components";
import { clock, empty, messages, open, page, sessions, thinking, title } from "./app";

// A column under the clock, as wide as the display allows up to the 280 px
// the design draws it at, centered so it keeps to the middle a round display
// shows. Messages wrap to the column and the newest stay at the bottom.
export default function Sessions() {
  return (
    <View class="w-full h-full flex-col items-center px-[16px] overflow-hidden bg-[#09090B]">
      <Text class="shrink-0 text-sm font-bold text-[#FAFAFA] mt-[16px]">{clock()}</Text>

      <Show when={page() === 0}>
        <View class="flex-1 min-h-0 w-full max-w-[280px] flex-col mt-[16px] overflow-hidden">
          <For each={sessions()} by={(session) => session.id}>
            {(session) => (
              <View class="shrink-0 w-full flex-col" focusable onPress={() => open(session().id)}>
                <View class="w-full min-h-[73px] flex-row flex-wrap items-center gap-3 px-3 py-[8px]">
                  <View class="shrink-0 w-2 h-2" />
                  <View class="basis-[72px] grow min-w-0 flex-col">
                    <Text class="text-base text-[#FAFAFA]">{session().title}</Text>
                    <Text class="text-base text-[#A1A1AA]">{session().detail}</Text>
                  </View>
                  <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{""}</Text>
                </View>
                <View class="w-full h-[1px] bg-[#27272A]" />
              </View>
            )}
          </For>
          <Show when={empty()}>
            <View class="w-full flex-1 flex-col items-center justify-center gap-2">
              <Text class="text-lg text-[#FAFAFA] text-center">No sessions yet</Text>
              <Text class="text-sm text-[#A1A1AA] text-center">Chats with the Agent show up here</Text>
            </View>
          </Show>
        </View>
      </Show>

      <Show when={page() === 1}>
        {/* The latest messages, the newest at the bottom; older ones go off the top. */}
        <View class="flex-1 min-h-0 w-full max-w-[280px] flex-col justify-end gap-[12px] mt-[12px] overflow-hidden">
          <For each={messages()} by={(message) => message.id}>
            {(message) => (
              <View class={message().mine ? "shrink-0 w-full flex-row justify-end pl-[40px]" : "shrink-0 w-full flex-col gap-[6px]"}>
                <Show when={message().mine}>
                  <View class="min-w-0 px-[14px] py-[8px] rounded-lg bg-[#18181B] border border-[#27272A]">
                    <Text class="min-w-0 text-base text-[#FAFAFA]">{message().text}</Text>
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
            <Text class="shrink-0 text-sm text-[#A1A1AA] animate-spin">{""}</Text>
          </Show>
        </View>
        <Text class="shrink-0 w-full max-w-[240px] text-base text-[#A1A1AA] text-center mt-[8px] mb-[14px]">{title()}</Text>
      </Show>
    </View>
  );
}
