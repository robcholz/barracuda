import { Show } from "solid-js";
import { For, Text, View } from "@pocketjs/framework/solid/components";
import {
  apps,
  appsShare,
  board,
  brightness,
  cacheShare,
  clock,
  connected,
  mediaShare,
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
} from "./app";

// Every page is a column under the clock, as wide as the display allows up to
// the 280 px the design draws it at and centered, so it keeps to the middle a
// round display shows; rows grow with text that wraps, bars fill by share
// rather than by pixels, and a page longer than the display is cut at the
// bottom. A right swipe goes back a page.
export default function Settings() {
  return (
    <View class="w-full h-full flex-col items-center px-[16px] overflow-hidden bg-[#09090B]">
      <Text class="shrink-0 text-sm font-bold text-[#FAFAFA] mt-[16px]">{clock()}</Text>

      <Show when={page() === 0}>
        <View class="flex-1 min-h-0 w-full max-w-[280px] flex-col mt-[16px] overflow-y-auto">
          <For each={rows()} by={(row) => row.id}>
            {(row) => (
              <View class="shrink-0 w-full flex-col" focusable onPress={() => open(row().id)}>
                <View class="w-full min-h-[57px] flex-row flex-wrap items-center gap-3 px-3 py-[6px]">
                  <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{row().glyph}</Text>
                  <Text class="basis-[72px] grow min-w-0 text-lg text-[#FAFAFA]">{row().label}</Text>
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
        <View class="flex-1 min-h-0 w-full max-w-[280px] flex-col mt-[16px] overflow-y-auto">
          <View class="shrink-0 w-full min-h-[57px] flex-row flex-wrap items-center gap-3 px-3 py-[6px]">
            <Text class={connected() ? "shrink-0 text-lg font-bold text-[#FAFAFA]" : "shrink-0 text-lg font-bold text-[#FF7878]"}>
              {connected() ? "" : ""}
            </Text>
            <Text class={named() ? "basis-[96px] grow min-w-0 font-mono text-base text-[#FAFAFA]" : "basis-[96px] grow min-w-0 text-lg text-[#FAFAFA]"}>{ssid()}</Text>
            <Show when={connected()}>
              <View class="shrink-0 h-6 px-2 rounded bg-[#CCFF00] flex-row items-center gap-[6px]">
                <View class="w-[6px] h-[6px] rounded-full bg-[#09090B]" />
                <Text class="text-sm text-[#09090B]">{network()}</Text>
              </View>
            </Show>
            <Show when={!connected()}>
              <Text class="shrink-0 text-sm text-[#A1A1AA]">{network()}</Text>
            </Show>
          </View>
          <View class="shrink-0 w-full h-[1px] bg-[#27272A]" />
          <View class="shrink-0 w-full flex-row items-center justify-between gap-2 px-3 pt-[18px] pb-[6px]">
            <Text class="flex-1 min-w-0 text-sm text-[#A1A1AA]">Nearby networks</Text>
            <Show when={scanning()}>
              <Text class="shrink-0 text-sm text-[#A1A1AA] animate-spin">{""}</Text>
            </Show>
          </View>
          <For each={networks()} by={(item) => item.id}>
            {(item) => (
              <View class="shrink-0 w-full flex-col">
                <View class="w-full min-h-[57px] flex-row flex-wrap items-center gap-3 px-3 py-[6px]">
                  <View class="shrink-0 w-[18px] h-[24px] relative">
                    <Text class="absolute left-0 top-0 text-lg font-bold text-[#52525B]">{item().signal.dim}</Text>
                    <Text class="absolute left-0 top-0 text-lg font-bold text-[#FAFAFA]">{item().signal.base}</Text>
                  </View>
                  <Text class="flex-1 min-w-0 font-mono text-base text-[#FAFAFA]">{item().ssid}</Text>
                  <Show when={item().locked}>
                    <Text class="shrink-0 text-base text-[#A1A1AA]">{""}</Text>
                  </Show>
                </View>
                <View class="w-full h-[1px] bg-[#27272A]" />
              </View>
            )}
          </For>
        </View>
      </Show>

      <Show when={page() === 2}>
        <View class="flex-1 min-h-0 w-full max-w-[280px] flex-col mt-[16px] overflow-y-auto">
          <View class="shrink-0 w-full flex-col px-3 pt-[10px] pb-[4px]">
            <View class="flex-row flex-wrap justify-between items-center gap-2">
              <Text class="basis-[72px] grow min-w-0 text-lg text-[#FAFAFA]">Volume</Text>
              <Text class="shrink-0 font-mono text-sm text-[#A1A1AA]">{volume()}%</Text>
            </View>
            <View class="w-full h-[28px] relative">
              <View class="absolute left-0 top-[10px] w-full h-[8px] rounded-[4px] bg-[#3F3F46] overflow-hidden flex-row">
                <View class="h-[8px] bg-[#FAFAFA]" style={{ grow: volume() }} />
                <View class="h-[8px]" style={{ grow: 100 - volume() }} />
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
          <View class="shrink-0 w-full h-[1px] bg-[#27272A]" />
          <View class="shrink-0 w-full flex-col px-3 pt-[10px] pb-[4px]">
            <View class="flex-row flex-wrap justify-between items-center gap-2">
              <Text class="basis-[72px] grow min-w-0 text-lg text-[#FAFAFA]">Brightness</Text>
              <Text class="shrink-0 font-mono text-sm text-[#A1A1AA]">{brightness()}%</Text>
            </View>
            <View class="w-full h-[28px] relative">
              <View class="absolute left-0 top-[10px] w-full h-[8px] rounded-[4px] bg-[#3F3F46] overflow-hidden flex-row">
                <View class="h-[8px] bg-[#FAFAFA]" style={{ grow: brightness() }} />
                <View class="h-[8px]" style={{ grow: 100 - brightness() }} />
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
          <View class="shrink-0 w-full h-[1px] bg-[#27272A]" />
          <View class="shrink-0 w-full flex-col" focusable onPress={() => nextTimeout()}>
            <View class="w-full min-h-[57px] flex-row flex-wrap items-center gap-3 px-3 py-[6px]">
              <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{""}</Text>
              <Text class="basis-[72px] grow min-w-0 text-lg text-[#FAFAFA]">Screen off</Text>
              <Text class="shrink-0 text-sm text-[#A1A1AA]">{timeout()}</Text>
            </View>
            <View class="w-full h-[1px] bg-[#27272A]" />
          </View>
          <View class="shrink-0 w-full flex-col" focusable onPress={() => toggleMute()}>
            <View class="w-full min-h-[57px] flex-row flex-wrap items-center gap-3 px-3 py-[6px]">
              <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{""}</Text>
              <Text class="basis-[72px] grow min-w-0 text-lg text-[#FAFAFA]">Mute</Text>
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
        <View class="flex-1 min-h-0 w-full max-w-[280px] flex-col mt-[16px] overflow-y-auto">
          <For each={apps()} by={(app) => app.id}>
            {(app) => (
              <View class="shrink-0 w-full flex-col">
                <View class="w-full min-h-[57px] flex-row flex-wrap items-center gap-3 px-3 py-[6px]">
                  <View class="shrink-0 w-[34px] h-[34px] rounded-[7px] bg-[#232327] items-center justify-center">
                    <View class="w-[18px] h-[24px] relative">
                      <Text class="absolute left-0 top-0 text-lg font-bold text-[#F4F4F5]">{app().icon.base}</Text>
                      <Text class="absolute left-0 top-0 text-lg font-bold text-[#CCFF00]">{app().icon.accent}</Text>
                    </View>
                    <Text class="absolute text-sm font-bold text-[#F4F4F5]">{app().letter}</Text>
                  </View>
                  <Text class="basis-[72px] grow min-w-0 text-lg text-[#FAFAFA]">{app().name}</Text>
                  <Show when={app().live}>
                    <View class="shrink-0 w-[6px] h-[6px] rounded-full bg-[#CCFF00]" />
                  </Show>
                  <Text class="shrink-0 text-sm text-[#A1A1AA]">{app().state}</Text>
                </View>
                <View class="w-full h-[1px] bg-[#27272A]" />
              </View>
            )}
          </For>
        </View>
      </Show>

      <Show when={page() === 4}>
        <View class="flex-1 min-h-0 w-full max-w-[280px] flex-col mt-[16px] overflow-y-auto">
          <View class="shrink-0 w-full flex-col px-3 pt-[6px] pb-[14px]">
            <Text class="text-2xl font-bold text-[#FAFAFA]">{used()}</Text>
            <Text class="text-sm text-[#A1A1AA]">{usedNote()}</Text>
            <View class="mt-[12px] w-full h-[8px] rounded-[4px] bg-[#3F3F46] overflow-hidden flex-row">
              <View class="h-[8px] bg-[#FAFAFA]" style={{ grow: mediaShare() }} />
              <View class="h-[8px] bg-[#A1A1AA]" style={{ grow: appsShare() }} />
              <View class="h-[8px] bg-[#71717A]" style={{ grow: cacheShare() }} />
              <View class="h-[8px]" style={{ grow: 1000 - mediaShare() - appsShare() - cacheShare() }} />
            </View>
          </View>
          <For each={rows()} by={(row) => row.id}>
            {(row) => (
              <View class="shrink-0 w-full flex-col">
                <View class="w-full h-[1px] bg-[#27272A]" />
                <View class="w-full min-h-[57px] flex-row flex-wrap items-center gap-3 px-3 py-[6px]">
                  <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{row().glyph}</Text>
                  <Text class="basis-[72px] grow min-w-0 text-lg text-[#FAFAFA]">{row().label}</Text>
                  <Text class="shrink-0 font-mono text-sm text-[#A1A1AA]">{row().value}</Text>
                </View>
              </View>
            )}
          </For>
        </View>
      </Show>

      <Show when={page() === 5}>
        <View class="flex-1 min-h-0 w-full max-w-[280px] flex-col items-center mt-[16px] overflow-y-auto">
          <View class="shrink-0 w-[60px] h-[48px] relative mt-[2px]">
            <Text class="absolute left-0 top-0 text-4xl text-[#CCFF00]">{""}</Text>
          </View>
          <Text class="shrink-0 mt-[10px] text-2xl font-bold text-[#FAFAFA]">Barracuda</Text>
          <Text class="shrink-0 text-sm text-[#A1A1AA] text-center mb-[10px]">{board()}</Text>
          <For each={rows()} by={(row) => row.id}>
            {(row) => (
              <View class="shrink-0 w-full flex-col">
                <View class="w-full h-[1px] bg-[#27272A]" />
                <View class="w-full min-h-[57px] flex-row flex-wrap items-center gap-3 px-3 py-[6px]">
                  <Text class="shrink-0 text-lg font-bold text-[#A1A1AA]">{row().glyph}</Text>
                  <Text class="basis-[72px] grow min-w-0 text-lg text-[#FAFAFA]">{row().label}</Text>
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
