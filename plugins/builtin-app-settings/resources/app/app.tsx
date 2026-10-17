import { Show } from "solid-js";
import { For, Text, View } from "@pocketjs/framework/solid/components";
import {
  apps,
  appsFill,
  board,
  brightness,
  brightnessFill,
  cacheFill,
  clock,
  connected,
  mediaFill,
  muted,
  named,
  network,
  networks,
  nextTimeout,
  open,
  page,
  rows,
  scanning,
  setBrightness,
  setVolume,
  ssid,
  timeout,
  toggleMute,
  used,
  usedNote,
  volume,
  volumeFill,
} from "./app";

// Every page is a column of 280 px rows from y = 52, inside the round
// screen's widest band; a right swipe goes back a page.
export default function Settings() {
  return (
    <View class="w-full h-full relative overflow-hidden bg-[#09090B]">
      <View class="absolute left-0 right-0 top-[16px] flex-row justify-center">
        <Text class="text-sm font-bold text-[#FAFAFA]">{clock()}</Text>
      </View>

      <Show when={page() === 0}>
        <View class="absolute left-[40px] right-[40px] top-[52px] flex-col">
          <For each={rows()} by={(row) => row.id}>
            {(row) => (
              <View class="w-full h-[58px] flex-col" focusable onPress={() => open(row().id)}>
                <View class="w-full h-[57px] flex-row items-center gap-3 px-3">
                  <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{row().glyph}</Text>
                  <Text class="flex-1 text-lg text-[#FAFAFA]">{row().label}</Text>
                  <Text class={row().mono ? "shrink-0 font-mono text-sm text-[#A1A1AA]" : "shrink-0 text-sm text-[#A1A1AA]"}>{row().value}</Text>
                  <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{""}</Text>
                </View>
                <View class="w-full h-[1px] bg-[#27272A]" />
              </View>
            )}
          </For>
        </View>
      </Show>

      <Show when={page() === 1}>
        <View class="absolute left-[40px] right-[40px] top-[52px] flex-col">
          <View class="w-full h-[57px] flex-row items-center gap-3 px-3">
            <Text class={connected() ? "text-lg font-bold text-[#FAFAFA]" : "text-lg font-bold text-[#FF7878]"}>
              {connected() ? "" : ""}
            </Text>
            <Text class={named() ? "flex-1 font-mono text-base text-[#FAFAFA]" : "flex-1 text-lg text-[#FAFAFA]"}>{ssid()}</Text>
            <Show when={connected()}>
              <View class="h-6 px-2 rounded bg-[#CCFF00] flex-row items-center gap-[6px]">
                <View class="w-[6px] h-[6px] rounded-full bg-[#09090B]" />
                <Text class="text-sm text-[#09090B]">{network()}</Text>
              </View>
            </Show>
            <Show when={!connected()}>
              <Text class="text-sm text-[#A1A1AA]">{network()}</Text>
            </Show>
          </View>
          <View class="w-full h-[1px] bg-[#27272A]" />
          <View class="w-full flex-row items-center justify-between px-3 pt-[18px] pb-[6px]">
            <Text class="text-sm text-[#A1A1AA]">附近的网络</Text>
            <Show when={scanning()}>
              <Text class="text-sm text-[#A1A1AA] animate-spin">{""}</Text>
            </Show>
          </View>
          <For each={networks()} by={(item) => item.id}>
            {(item) => (
              <View class="w-full h-[58px] flex-col">
                <View class="w-full h-[57px] flex-row items-center gap-3 px-3">
                  <View class="w-[18px] h-[24px] relative">
                    <Text class="absolute left-0 top-0 text-lg font-bold text-[#52525B]">{item().signal.dim}</Text>
                    <Text class="absolute left-0 top-0 text-lg font-bold text-[#FAFAFA]">{item().signal.base}</Text>
                  </View>
                  <Text class="flex-1 font-mono text-base text-[#FAFAFA]">{item().ssid}</Text>
                  <Show when={item().locked}>
                    <Text class="text-base text-[#A1A1AA]">{""}</Text>
                  </Show>
                </View>
                <View class="w-full h-[1px] bg-[#27272A]" />
              </View>
            )}
          </For>
        </View>
      </Show>

      <Show when={page() === 2}>
        <View class="absolute left-[40px] right-[40px] top-[52px] flex-col">
          <View class="w-full flex-col px-3 pt-[10px] pb-[4px]">
            <View class="flex-row justify-between items-center">
              <Text class="text-lg text-[#FAFAFA]">音量</Text>
              <Text class="shrink-0 font-mono text-sm text-[#A1A1AA]">{volume()}%</Text>
            </View>
            <View class="w-full h-[28px] relative">
              <View class="absolute left-0 top-[10px] w-full h-[8px] rounded-[4px] bg-[#3F3F46] overflow-hidden">
                <View class="h-[8px] bg-[#FAFAFA]" style={{ width: volumeFill() }} />
              </View>
              <View class="absolute left-0 top-0 w-full h-[28px] flex-row">
                <View class="flex-1 h-full" focusable onPress={() => setVolume(0)} />
                <View class="flex-1 h-full" focusable onPress={() => setVolume(20)} />
                <View class="flex-1 h-full" focusable onPress={() => setVolume(30)} />
                <View class="flex-1 h-full" focusable onPress={() => setVolume(40)} />
                <View class="flex-1 h-full" focusable onPress={() => setVolume(50)} />
                <View class="flex-1 h-full" focusable onPress={() => setVolume(60)} />
                <View class="flex-1 h-full" focusable onPress={() => setVolume(70)} />
                <View class="flex-1 h-full" focusable onPress={() => setVolume(80)} />
                <View class="flex-1 h-full" focusable onPress={() => setVolume(90)} />
                <View class="flex-1 h-full" focusable onPress={() => setVolume(100)} />
              </View>
            </View>
          </View>
          <View class="w-full h-[1px] bg-[#27272A]" />
          <View class="w-full flex-col px-3 pt-[10px] pb-[4px]">
            <View class="flex-row justify-between items-center">
              <Text class="text-lg text-[#FAFAFA]">亮度</Text>
              <Text class="shrink-0 font-mono text-sm text-[#A1A1AA]">{brightness()}%</Text>
            </View>
            <View class="w-full h-[28px] relative">
              <View class="absolute left-0 top-[10px] w-full h-[8px] rounded-[4px] bg-[#3F3F46] overflow-hidden">
                <View class="h-[8px] bg-[#FAFAFA]" style={{ width: brightnessFill() }} />
              </View>
              <View class="absolute left-0 top-0 w-full h-[28px] flex-row">
                <View class="flex-1 h-full" focusable onPress={() => setBrightness(10)} />
                <View class="flex-1 h-full" focusable onPress={() => setBrightness(20)} />
                <View class="flex-1 h-full" focusable onPress={() => setBrightness(30)} />
                <View class="flex-1 h-full" focusable onPress={() => setBrightness(40)} />
                <View class="flex-1 h-full" focusable onPress={() => setBrightness(50)} />
                <View class="flex-1 h-full" focusable onPress={() => setBrightness(60)} />
                <View class="flex-1 h-full" focusable onPress={() => setBrightness(70)} />
                <View class="flex-1 h-full" focusable onPress={() => setBrightness(80)} />
                <View class="flex-1 h-full" focusable onPress={() => setBrightness(90)} />
                <View class="flex-1 h-full" focusable onPress={() => setBrightness(100)} />
              </View>
            </View>
          </View>
          <View class="w-full h-[1px] bg-[#27272A]" />
          <View class="w-full h-[58px] flex-col" focusable onPress={() => nextTimeout()}>
            <View class="w-full h-[57px] flex-row items-center gap-3 px-3">
              <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{""}</Text>
              <Text class="flex-1 text-lg text-[#FAFAFA]">息屏</Text>
              <Text class="text-sm text-[#A1A1AA]">{timeout()}</Text>
            </View>
            <View class="w-full h-[1px] bg-[#27272A]" />
          </View>
          <View class="w-full h-[58px] flex-col" focusable onPress={() => toggleMute()}>
            <View class="w-full h-[57px] flex-row items-center gap-3 px-3">
              <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{""}</Text>
              <Text class="flex-1 text-lg text-[#FAFAFA]">静音</Text>
              <View
                class={
                  muted()
                    ? "shrink-0 w-[46px] h-[28px] rounded-full bg-[#FAFAFA] flex-row items-center justify-end px-[3px]"
                    : "shrink-0 w-[46px] h-[28px] rounded-full bg-[#27272A] border border-[#3F3F46] flex-row items-center justify-start px-[3px]"
                }
              >
                <View class={muted() ? "w-[22px] h-[22px] rounded-full bg-[#09090B]" : "w-[20px] h-[20px] rounded-full bg-[#09090B]"} />
              </View>
            </View>
            <View class="w-full h-[1px] bg-[#27272A]" />
          </View>
        </View>
      </Show>

      <Show when={page() === 3}>
        <View class="absolute left-[40px] right-[40px] top-[52px] flex-col">
          <For each={apps()} by={(app) => app.id}>
            {(app) => (
              <View class="w-full h-[58px] flex-col">
                <View class="w-full h-[57px] flex-row items-center gap-3 px-3">
                  <View class="w-[34px] h-[34px] rounded-[7px] bg-[#232327] items-center justify-center">
                    <View class="w-[18px] h-[24px] relative">
                      <Text class="absolute left-0 top-0 text-lg font-bold text-[#F4F4F5]">{app().icon.base}</Text>
                      <Text class="absolute left-0 top-0 text-lg font-bold text-[#CCFF00]">{app().icon.accent}</Text>
                    </View>
                    <Text class="absolute text-sm font-bold text-[#F4F4F5]">{app().letter}</Text>
                  </View>
                  <Text class="flex-1 text-lg text-[#FAFAFA]">{app().name}</Text>
                  <Show when={app().live}>
                    <View class="w-[6px] h-[6px] rounded-full bg-[#CCFF00]" />
                  </Show>
                  <Text class="text-sm text-[#A1A1AA]">{app().state}</Text>
                </View>
                <View class="w-full h-[1px] bg-[#27272A]" />
              </View>
            )}
          </For>
        </View>
      </Show>

      <Show when={page() === 4}>
        <View class="absolute left-[40px] right-[40px] top-[52px] flex-col">
          <View class="w-full flex-col px-3 pt-[6px] pb-[14px]">
            <Text class="text-2xl font-bold text-[#FAFAFA]">{used()}</Text>
            <Text class="text-sm text-[#A1A1AA]">{usedNote()}</Text>
            <View class="mt-[12px] w-full h-[8px] rounded-[4px] bg-[#3F3F46] overflow-hidden flex-row">
              <View class="h-[8px] bg-[#FAFAFA]" style={{ width: mediaFill() }} />
              <View class="h-[8px] bg-[#A1A1AA]" style={{ width: appsFill() }} />
              <View class="h-[8px] bg-[#71717A]" style={{ width: cacheFill() }} />
            </View>
          </View>
          <For each={rows()} by={(row) => row.id}>
            {(row) => (
              <View class="w-full h-[58px] flex-col">
                <View class="w-full h-[1px] bg-[#27272A]" />
                <View class="w-full h-[57px] flex-row items-center gap-3 px-3">
                  <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{row().glyph}</Text>
                  <Text class="flex-1 text-lg text-[#FAFAFA]">{row().label}</Text>
                  <Text class="shrink-0 font-mono text-sm text-[#A1A1AA]">{row().value}</Text>
                </View>
              </View>
            )}
          </For>
        </View>
      </Show>

      <Show when={page() === 5}>
        <View class="absolute left-[40px] right-[40px] top-[52px] flex-col items-center">
          <View class="w-[60px] h-[48px] relative mt-[2px]">
            <Text class="absolute left-0 top-0 text-4xl text-[#CCFF00]">{""}</Text>
          </View>
          <Text class="mt-[10px] text-2xl font-bold text-[#FAFAFA]">Barracuda</Text>
          <Text class="text-sm text-[#A1A1AA] mb-[10px]">{board()}</Text>
          <For each={rows()} by={(row) => row.id}>
            {(row) => (
              <View class="w-full h-[58px] flex-col">
                <View class="w-full h-[1px] bg-[#27272A]" />
                <View class="w-full h-[57px] flex-row items-center gap-3 px-3">
                  <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{row().glyph}</Text>
                  <Text class="flex-1 text-lg text-[#FAFAFA]">{row().label}</Text>
                  <Text class={row().mono ? "shrink-0 font-mono text-sm text-[#A1A1AA]" : "shrink-0 text-sm text-[#A1A1AA]"}>{row().value}</Text>
                </View>
              </View>
            )}
          </For>
        </View>
      </Show>
    </View>
  );
}
