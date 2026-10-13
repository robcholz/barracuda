import { Show } from "solid-js";
import { For, Text, View } from "@pocketjs/framework/solid/components";
import {
  alert,
  alertDetail,
  alertTap,
  apps,
  cancel,
  clearAll,
  clock,
  closeApp,
  confirm,
  detail,
  dismiss,
  glyph,
  headline,
  hint,
  lines,
  notices,
  openApp,
  primary,
  running,
  screen,
  secondary,
  switchTo,
  tiles,
  title,
  toggle,
  unread,
} from "./app";

// One view for every system screen; `screen` picks which one is mounted.
// Colors are the portal's dark tokens: background #09090B, border #27272A,
// input #63636B, muted text #A1A1AA, foreground #FAFAFA, signal #CCFF00,
// warning #ED8E04.
export default function System() {
  return (
    <View class="w-full h-full flex-col items-center bg-[#09090B]">
      <View class="w-full flex-row items-center justify-center gap-2 pt-2 h-7">
        <Show when={unread()}>
          <View class="w-2 h-2 rounded-full bg-[#FAFAFA]" />
        </Show>
        <Text class="text-base text-[#FAFAFA]">{clock()}</Text>
      </View>

      <Show when={screen() === 0}>
        <View class="flex-1 w-full flex-col items-center justify-center gap-1">
          <View class="w-16 h-16 rounded-xl bg-[#CCFF00] items-center justify-center">
            <Text class="text-2xl font-bold text-[#09090B]">B</Text>
          </View>
          <Text class="text-2xl font-bold text-[#FAFAFA] pt-2">{headline()}</Text>
          <Text class="text-base text-[#A1A1AA]">{hint()}</Text>
          <Show when={alert() !== ""}>
            <View
              class="flex-row items-center gap-3 mt-3 px-3 py-2 rounded-lg border border-[#27272A] focus:bg-[#18181B]"
              focusable
              onPress={() => alertTap()}
            >
              <View class="w-2 h-2 rounded-full bg-[#ED8E04]" />
              <View class="flex-col">
                <Text class="text-base text-[#A1A1AA]">{alertDetail()}</Text>
                <Text class="text-base text-[#FAFAFA]">{alert()}</Text>
              </View>
            </View>
          </Show>
        </View>
      </Show>

      <Show when={screen() === 1}>
        <View class="flex-1 w-full flex-col justify-center">
          <View class="w-full flex-row flex-wrap justify-center gap-2 px-4">
            <For each={apps()} by={(app) => app.id}>
              {(app) => (
                <View
                  class="w-16 flex-col items-center gap-1 rounded-lg focus:bg-[#18181B]"
                  focusable
                  onPress={() => openApp(app().id)}
                >
                  <View class="w-12 h-12 rounded-xl bg-[#232327] items-center justify-center">
                    <Text class="text-base text-[#F4F4F5]">{app().glyph}</Text>
                  </View>
                  <View class="flex-row items-center gap-1">
                    <Show when={app().running}>
                      <View class="w-1.5 h-1.5 rounded-full bg-[#CCFF00]" />
                    </Show>
                    <Text class={app().offered ? "text-base text-[#A1A1AA]" : "text-base text-[#FAFAFA]"}>
                      {app().name}
                    </Text>
                  </View>
                </View>
              )}
            </For>
          </View>
        </View>
      </Show>

      <Show when={screen() === 2}>
        <View class="flex-1 w-full flex-col items-center justify-center gap-2 px-6">
          <Text class="text-base text-[#FAFAFA]">{title()}</Text>
          <For each={running()} by={(app) => app.id}>
            {(app) => (
              <View class="w-full flex-row items-center gap-3 px-3 py-2 rounded-lg border border-[#27272A]">
                <View
                  class="flex-1 flex-row items-center gap-3 focus:bg-[#18181B]"
                  focusable
                  onPress={() => switchTo(app().id)}
                >
                  <View class="w-8 h-8 rounded-md bg-[#232327] items-center justify-center">
                    <Text class="text-base text-[#F4F4F5]">{app().glyph}</Text>
                  </View>
                  <View class="flex-col">
                    <Text class={app().active ? "text-base text-[#FAFAFA]" : "text-base text-[#A1A1AA]"}>
                      {app().name}
                    </Text>
                    <Text class="text-base text-[#A1A1AA]">{app().detail}</Text>
                  </View>
                </View>
                <View
                  class="w-8 h-8 rounded-md items-center justify-center focus:bg-[#27272A]"
                  focusable
                  onPress={() => closeApp(app().id)}
                >
                  <Text class="text-base text-[#A1A1AA]">×</Text>
                </View>
              </View>
            )}
          </For>
          <Text class="text-base text-[#A1A1AA]">{detail()}</Text>
        </View>
      </Show>

      <Show when={screen() === 3}>
        <View class="flex-1 w-full flex-col items-center gap-2 px-6 pt-1">
          <Text class="text-base text-[#FAFAFA]">{title()}</Text>
          <For each={notices()} by={(notice) => notice.id}>
            {(notice) => (
              <View class="w-full flex-row items-center gap-2 px-3 py-2 rounded-lg border border-[#27272A]">
                <View class="flex-1 flex-col">
                  <Text class="text-base text-[#FAFAFA]">{notice().title}</Text>
                  <Text class="text-base text-[#A1A1AA]">{notice().body}</Text>
                </View>
                <View
                  class="w-8 h-8 rounded-md items-center justify-center focus:bg-[#27272A]"
                  focusable
                  onPress={() => dismiss(notice().id)}
                >
                  <Text class="text-base text-[#A1A1AA]">×</Text>
                </View>
              </View>
            )}
          </For>
          <View class="px-3 py-2 rounded-md focus:bg-[#18181B]" focusable onPress={() => clearAll()}>
            <Text class="text-base text-[#A1A1AA]">{detail()}</Text>
          </View>
        </View>
      </Show>

      <Show when={screen() === 4}>
        <View class="flex-1 w-full flex-col justify-center">
          <View class="w-full flex-row flex-wrap justify-center gap-2 px-6">
            <For each={tiles()} by={(tile) => tile.id}>
              {(tile) => (
                <View
                  class={
                    tile().on
                      ? "w-32 h-14 flex-col justify-center px-3 rounded-md bg-[#FAFAFA]"
                      : "w-32 h-14 flex-col justify-center px-3 rounded-md border border-[#63636B]"
                  }
                  focusable
                  onPress={() => toggle(tile().id)}
                >
                  <Text class={tile().on ? "text-base text-[#09090B]" : "text-base text-[#FAFAFA]"}>
                    {tile().label}
                  </Text>
                  <Text class={tile().on ? "text-base text-[#09090B]" : "text-base text-[#A1A1AA]"}>
                    {tile().detail}
                  </Text>
                </View>
              )}
            </For>
          </View>
        </View>
      </Show>

      <Show when={screen() >= 5}>
        <View class="flex-1 w-full flex-col items-center justify-center gap-1 px-6">
          <View
            class={
              screen() === 5
                ? "w-20 h-20 rounded-xl bg-[#232327] items-center justify-center"
                : "w-12 h-12 rounded-xl bg-[#232327] items-center justify-center"
            }
          >
            <Text class="text-base text-[#F4F4F5]">{glyph()}</Text>
          </View>
          <Text class="text-base text-[#FAFAFA] pt-2">{title()}</Text>
          <Text class="text-base text-[#A1A1AA]">{detail()}</Text>
          <For each={lines()} by={(line) => line.id}>
            {(line) => <Text class="text-base text-[#FAFAFA]">{line().text}</Text>}
          </For>
          <View class="flex-row gap-2 pt-3">
            <Show when={secondary() !== ""}>
              <View
                class="px-4 py-2 rounded-md border border-[#63636B] focus:bg-[#18181B]"
                focusable
                onPress={() => cancel()}
              >
                <Text class="text-base text-[#FAFAFA]">{secondary()}</Text>
              </View>
            </Show>
            <Show when={primary() !== ""}>
              <View
                class={
                  screen() === 8
                    ? "px-4 py-2 rounded-md bg-[#FF7878] focus:bg-[#E86A6A]"
                    : "px-4 py-2 rounded-md bg-[#CCFF00] focus:bg-[#BAE634]"
                }
                focusable
                onPress={() => confirm()}
              >
                <Text class="text-base text-[#09090B]">{primary()}</Text>
              </View>
            </Show>
          </View>
        </View>
      </Show>
    </View>
  );
}
