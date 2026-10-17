import { Show } from "solid-js";
import { For, Text, View } from "@pocketjs/framework/solid/components";
import {
  agent,
  allow,
  anyNotice,
  anyRunning,
  answer,
  apps,
  battery,
  batteryFill,
  blocked,
  brightness,
  brightnessFill,
  cancel,
  card,
  cardGlyph,
  cardLabel,
  cardTap,
  cardTone,
  clearAll,
  clock,
  closeApp,
  closeControl,
  confirm,
  control,
  deny,
  detail,
  dots,
  dismiss,
  done,
  icon,
  info,
  letter,
  lines,
  notices,
  offline,
  openApp,
  portal,
  primary,
  question,
  quiet,
  removable,
  running,
  screen,
  secondary,
  setBrightness,
  switchTo,
  tiles,
  title,
  toggle,
  unread,
} from "./app";

// The device UI's system screens, laid out as the design draws them for the
// 360 px round display and sized by the display they show on: full-screen
// layers fill it, columns keep their margins, and the slides between screens
// fade as they move so a larger display never shows a layer that left. Icons are glyphs of the system icon font, one Text per
// color layer stacked in place. Colors are the portal's dark tokens:
// background #09090B, muted #18181B, border #27272A, input #63636B, muted text
// #A1A1AA, foreground #FAFAFA, signal #CCFF00, warning #ED8E04, destructive
// #FF7878; app tiles are #232327.
export default function System() {
  return (
    <View class="w-full h-full relative overflow-hidden bg-[#09090B]">
      {/* Home, sliding left when the launcher comes in. */}
      <View
        class={
          screen() === 1 || screen() === 2 || screen() === 9
            ? "absolute left-0 top-0 w-full h-full translate-x-[-360px] opacity-0 transition-all duration-280 ease-out"
            : "absolute left-0 top-0 w-full h-full translate-x-0 opacity-100 transition-all duration-280 ease-out"
        }
      >
        <View class="absolute left-0 right-0 top-[16px] flex-row justify-center items-center gap-2">
          <Text class="text-sm font-bold text-[#FAFAFA]">{clock()}</Text>
          <Show when={quiet()}>
            <Text class="text-base text-[#A1A1AA]">{"\uE058"}</Text>
          </Show>
          <Show when={offline()}>
            <Text class="text-base text-[#FF7878]">{"\uE0A0"}</Text>
          </Show>
        </View>
        <Show when={unread()}>
          <View class="absolute left-0 right-0 top-[6px] flex-row justify-center">
            <View class="w-2 h-2 rounded-full bg-[#FAFAFA]" />
          </View>
        </Show>

        <Show when={agent() === 0}>
          <View class="absolute left-0 top-0 w-full h-full flex-col items-center justify-center pt-[6px]">
            <Text class="text-5xl font-bold text-[#CCFF00] scale-150 my-[14px]">{"\uE008"}</Text>
            <Text class="text-2xl font-bold text-[#FAFAFA]">待命</Text>
            <Text class="text-sm text-[#A1A1AA]">按住 Agent 键说话</Text>
            <Show when={card() !== ""}>
              <View class="flex-row items-center gap-3 mt-[20px] pl-[8px] pr-[10px] py-[8px] rounded-lg" focusable onPress={() => cardTap()}>
                <View class="w-8 h-8 rounded-[7px] bg-[#232327] items-center justify-center">
                  <Text class={cardTone() === 1 ? "text-base text-[#ED8E04]" : cardTone() === 2 ? "text-base text-[#FF7878]" : "text-base text-[#F4F4F5]"}>
                    {cardGlyph()}
                  </Text>
                </View>
                <View class="flex-col">
                  <Text class="text-sm text-[#A1A1AA]">{cardLabel()}</Text>
                  <Text class="text-base text-[#FAFAFA]">{card()}</Text>
                </View>
                <Text class="text-base text-[#A1A1AA]">{"\uE028"}</Text>
              </View>
            </Show>
          </View>
        </Show>
        <Show when={agent() === 1}>
          <View class="absolute left-0 top-0 w-full h-full flex-col items-center justify-center pt-[6px]">
            <Text class="text-5xl font-bold text-[#CCFF00] scale-150 my-[14px]">{"\uE008"}</Text>
            <View class="flex-row items-end gap-1 mt-[4px] h-[20px]">
              <View class="w-1 h-[8px] rounded-full bg-[#CCFF00] animate-pulse" />
              <View class="w-1 h-[16px] rounded-full bg-[#CCFF00] animate-pulse" />
              <View class="w-1 h-[20px] rounded-full bg-[#CCFF00] animate-pulse" />
              <View class="w-1 h-[12px] rounded-full bg-[#CCFF00] animate-pulse" />
              <View class="w-1 h-[6px] rounded-full bg-[#CCFF00] animate-pulse" />
            </View>
            <Text class="text-base text-[#FAFAFA] mt-[14px]">{question()}</Text>
          </View>
        </Show>
        <Show when={agent() === 2}>
          <View class="absolute left-0 top-0 w-full h-full flex-col items-center justify-center pt-[6px]">
            <Text class="text-5xl font-bold text-[#CCFF00] scale-150 my-[14px] animate-pulse">{"\uE008"}</Text>
            <Text class="text-base text-[#A1A1AA] mt-[6px]">{question()}</Text>
            <Text class="text-base text-[#FAFAFA]">思考中</Text>
          </View>
        </Show>
        <Show when={agent() === 3}>
          <View class="absolute left-0 top-0 w-full h-full flex-col items-center justify-center pt-[6px] px-[56px]" focusable onPress={() => cardTap()}>
            <Text class="text-5xl font-bold text-[#CCFF00] scale-150 my-[14px]">{"\uE008"}</Text>
            <Text class="text-base text-[#A1A1AA] mt-[6px]">{question()}</Text>
            <Text class="text-base text-[#FAFAFA] text-center mt-[4px]">{answer()}</Text>
          </View>
        </Show>
      </View>

      {/* The launcher: pages of 2-3-2 app tiles placed by the Plugin. */}
      <View
        class={
          screen() === 1 || screen() === 9
            ? "absolute left-0 top-0 w-full h-full translate-x-0 opacity-100 transition-all duration-280 ease-out"
            : screen() === 2
              ? "absolute left-0 top-0 w-full h-full translate-x-[-360px] opacity-0 transition-all duration-280 ease-out"
              : "absolute left-0 top-0 w-full h-full translate-x-[360px] opacity-0 transition-all duration-280 ease-out"
        }
      >
        <View class={screen() === 9 ? "absolute left-0 top-0 w-full h-full opacity-20" : "absolute left-0 top-0 w-full h-full"}>
          <For each={apps()} by={(app) => app.id}>
            {(app) => (
              <View
                class="absolute w-[78px] flex-col items-center gap-[6px]"
                style={{ insetL: app().x, insetT: app().y }}
                focusable
                onPress={() => openApp(app().id)}
              >
                <View class="w-16 h-16 rounded-[14px] bg-[#232327] items-center justify-center">
                  <View class="w-[36px] h-[44px] relative">
                    <Text class="absolute left-0 top-0 text-4xl text-[#52525B]">{app().icon.dim}</Text>
                    <Text class="absolute left-0 top-0 text-4xl text-[#F4F4F5]">{app().icon.base}</Text>
                    <Text class="absolute left-0 top-0 text-4xl text-[#CCFF00]">{app().icon.accent}</Text>
                  </View>
                  <Text class="absolute text-lg font-bold text-[#F4F4F5]">{app().letter}</Text>
                </View>
                <View class="flex-row items-center gap-[5px]">
                  <Show when={app().running}>
                    <View class="w-[6px] h-[6px] rounded-full bg-[#CCFF00]" />
                  </Show>
                  <Text class={app().offered ? "text-sm text-[#A1A1AA]" : "text-sm text-[#FAFAFA]"}>{app().name}</Text>
                </View>
              </View>
            )}
          </For>
          <View class="absolute right-[14px] top-0 bottom-0 flex-col justify-center gap-[6px]">
            <For each={dots()} by={(dot) => dot.id}>
              {(dot) => <View class={dot().on ? "w-[6px] h-[6px] rounded-full bg-[#CCFF00]" : "w-[6px] h-[6px] rounded-full bg-[#52525B]"} />}
            </For>
          </View>
        </View>
        <Show when={screen() === 9}>
          <View class="absolute left-0 top-0 w-full h-full flex-col items-center justify-center gap-[10px]">
            <Text class="text-lg font-bold text-[#FAFAFA] mb-[4px]">{title()}</Text>
            <View class="w-[200px] h-[44px] rounded-md border border-[#63636B] bg-[#09090B] items-center justify-center" focusable onPress={() => info()}>
              <Text class="text-base text-[#FAFAFA]">应用信息</Text>
            </View>
            <Show when={removable()}>
              <View class="w-[200px] h-[44px] rounded-md bg-[#FF7878] items-center justify-center" focusable onPress={() => confirm()}>
                <Text class="text-base text-[#09090B]">卸载</Text>
              </View>
            </Show>
          </View>
        </Show>
      </View>

      {/* The app switcher, left of the launcher. */}
      <View
        class={
          screen() === 2
            ? "absolute left-0 top-0 w-full h-full translate-x-0 opacity-100 transition-all duration-280 ease-out"
            : "absolute left-0 top-0 w-full h-full translate-x-[360px] opacity-0 transition-all duration-280 ease-out"
        }
      >
        <View class="absolute left-0 right-0 top-[16px] flex-row justify-center">
          <Text class="text-sm font-bold text-[#FAFAFA]">运行中</Text>
        </View>
        <View class="absolute left-[38px] right-[38px] top-[56px] flex-col gap-2">
          <For each={running()} by={(app) => app.id}>
            {(app) => (
              <View class="w-full h-[60px] rounded-lg border border-[#27272A] bg-[#09090B] flex-row items-center gap-[10px] pl-[16px] pr-[6px]">
                <View class="flex-1 flex-row items-center gap-[10px] h-[58px]" focusable onPress={() => switchTo(app().id)}>
                  <View class="w-[30px] h-[30px] rounded-[7px] bg-[#232327] items-center justify-center">
                    <View class="w-[16px] h-[22px] relative">
                      <Text class="absolute left-0 top-0 text-base text-[#F4F4F5]">{app().icon.base}</Text>
                      <Text class="absolute left-0 top-0 text-base text-[#CCFF00]">{app().icon.accent}</Text>
                    </View>
                    <Text class="absolute text-sm font-bold text-[#F4F4F5]">{app().letter}</Text>
                  </View>
                  <Text class={app().active ? "flex-1 text-lg font-bold text-[#FAFAFA]" : "flex-1 text-lg font-bold text-[#A1A1AA]"}>{app().name}</Text>
                  <Show when={app().active}>
                    <View class="h-6 px-2 rounded bg-[#CCFF00] flex-row items-center gap-[6px]">
                      <View class="w-[6px] h-[6px] rounded-full bg-[#09090B]" />
                      <Text class="text-sm text-[#09090B]">{app().detail}</Text>
                    </View>
                  </Show>
                  <Show when={!app().active}>
                    <Text class="text-sm text-[#A1A1AA]">{app().detail}</Text>
                  </Show>
                </View>
                <View class="w-8 h-8 items-center justify-center rounded-md" focusable onPress={() => closeApp(app().id)}>
                  <Text class="text-base text-[#A1A1AA]">{"\uE0A8"}</Text>
                </View>
              </View>
            )}
          </For>
          <Show when={!anyRunning()}>
            <View class="w-full h-[200px] items-center justify-center">
              <Text class="text-sm text-[#A1A1AA]">没有运行中的应用</Text>
            </View>
          </Show>
        </View>
      </View>

      {/* Launching, exited, install, and app info: one centered column. */}
      <Show when={screen() >= 5 && screen() <= 8}>
        <View class="absolute left-0 top-0 w-full h-full bg-[#09090B] flex-col items-center justify-center">
          <View
            class={
              screen() === 5
                ? "w-[120px] h-[120px] rounded-[26px] bg-[#232327] items-center justify-center"
                : screen() === 6
                  ? "w-16 h-16 rounded-[14px] bg-[#232327] items-center justify-center"
                  : "w-12 h-12 rounded-[11px] bg-[#232327] items-center justify-center"
            }
          >
            <View class="w-[36px] h-[44px] relative">
              <Text class="absolute left-0 top-0 text-4xl text-[#F4F4F5]">{icon().base}</Text>
              <Text class="absolute left-0 top-0 text-4xl text-[#CCFF00]">{icon().accent}</Text>
            </View>
            <Text class="absolute text-lg font-bold text-[#F4F4F5]">{letter()}</Text>
          </View>
          <Show when={screen() === 5}>
            <Text class="text-lg font-bold text-[#FAFAFA] mt-[14px]">{title()}</Text>
            <Text class="text-lg font-bold text-[#A1A1AA] mt-[12px] animate-spin">{"\uE048"}</Text>
          </Show>
          <Show when={screen() === 6}>
            <View class="flex-row items-center gap-2 mt-[14px]">
              <Text class="text-lg font-bold text-[#ED8E04]">{"\uE010"}</Text>
              <Text class="text-lg font-bold text-[#FAFAFA]">{title()}</Text>
            </View>
            <Text class="text-sm text-[#A1A1AA] mt-[4px]">{detail()}</Text>
            <View class="w-[224px] h-12 rounded-md bg-[#CCFF00] items-center justify-center mt-[20px]" focusable onPress={() => confirm()}>
              <Text class="text-lg font-bold text-[#09090B]">{primary()}</Text>
            </View>
            <View class="w-[184px] h-12 rounded-md border border-[#63636B] items-center justify-center mt-[8px]" focusable onPress={() => cancel()}>
              <Text class="text-lg font-bold text-[#FAFAFA]">{secondary()}</Text>
            </View>
          </Show>
          <Show when={screen() === 7 || screen() === 8}>
            <Text class="text-lg font-bold text-[#FAFAFA] mt-[8px]">{title()}</Text>
            <Text class="text-sm text-[#A1A1AA]">{detail()}</Text>
            <View class="w-[256px] flex-col mt-[8px]">
              <For each={lines()} by={(line) => line.id}>
                {(line) => (
                  <View class="w-[256px] flex-col">
                    <View class="w-[256px] h-[1px] bg-[#27272A]" />
                    <View class="w-[256px] h-[50px] flex-row items-center gap-3 px-3">
                      <Text class="text-lg font-bold text-[#A1A1AA]">{line().glyph}</Text>
                      <Text class="text-lg font-bold text-[#FAFAFA]">{line().text}</Text>
                    </View>
                  </View>
                )}
              </For>
              <View class="w-[256px] h-[1px] bg-[#27272A]" />
            </View>
            <View class="flex-row gap-2 mt-[10px]">
              <View class="w-[100px] h-12 rounded-md border border-[#63636B] items-center justify-center" focusable onPress={() => cancel()}>
                <Text class="text-lg font-bold text-[#FAFAFA]">{secondary()}</Text>
              </View>
              <Show when={primary() !== "" && !blocked()}>
                <View
                  class={screen() === 8 ? "w-[124px] h-12 rounded-md bg-[#FF7878] items-center justify-center" : "w-[124px] h-12 rounded-md bg-[#CCFF00] items-center justify-center"}
                  focusable
                  onPress={() => confirm()}
                >
                  <Text class="text-lg font-bold text-[#09090B]">{primary()}</Text>
                </View>
              </Show>
              <Show when={blocked()}>
                <View class="w-[124px] h-12 rounded-md bg-[#27272A] items-center justify-center">
                  <Text class="text-lg font-bold text-[#63636B]">{primary()}</Text>
                </View>
              </Show>
            </View>
          </Show>
        </View>
      </Show>

      {/* The notification center, pulled down from the top edge. */}
      <View
        class={
          screen() === 3
            ? "absolute left-0 top-0 w-full h-full bg-[#09090B] translate-y-0 opacity-100 transition-all duration-280 ease-out"
            : "absolute left-0 top-0 w-full h-full bg-[#09090B] translate-y-[-360px] opacity-0 transition-all duration-280 ease-out"
        }
      >
        <View class="absolute left-0 right-0 top-[22px] flex-row justify-center">
          <Text class="text-lg font-bold text-[#FAFAFA]">通知</Text>
        </View>
        <View class="absolute left-[54px] right-[54px] top-[58px] flex-col gap-2">
          <Show when={done() !== ""}>
            <View class="w-full rounded-lg border border-[#27272A] flex-row items-center gap-2 px-[14px] py-[10px]">
              <Text class="text-base text-[#FAFAFA]">{"\uE020"}</Text>
              <Text class="text-base text-[#FAFAFA]">{done()}</Text>
            </View>
          </Show>
          <For each={notices()} by={(notice) => notice.id}>
            {(notice) => (
              <View class="w-full rounded-lg border border-[#27272A] flex-col">
                <Show when={notice().confirm}>
                  <View class="flex-col px-[14px] pt-[12px] pb-[10px]">
                    <View class="flex-row items-center gap-2">
                      <Text class="text-base text-[#ED8E04]">{"\uE070"}</Text>
                      <Text class="text-base text-[#FAFAFA]">{notice().title}</Text>
                    </View>
                    <Text class="text-base text-[#A1A1AA] mt-[2px]">{notice().body}</Text>
                  </View>
                  <View class="w-full h-[1px] bg-[#27272A]" />
                  <View class="flex-row gap-2 px-[14px] py-[10px]">
                    <View class="w-[94px] h-10 rounded-md border border-[#63636B] items-center justify-center" focusable onPress={() => deny(notice().id)}>
                      <Text class="text-sm font-bold text-[#FAFAFA]">{notice().deny}</Text>
                    </View>
                    <View class="w-[124px] h-10 rounded-md bg-[#CCFF00] items-center justify-center" focusable onPress={() => allow(notice().id)}>
                      <Text class="text-sm font-bold text-[#09090B]">{notice().allow}</Text>
                    </View>
                  </View>
                </Show>
                <Show when={!notice().confirm}>
                  <View class="flex-row items-center gap-[10px] pl-[12px] pr-[8px] py-[10px]">
                    <View class="w-7 h-7 rounded-md bg-[#232327] items-center justify-center">
                      <View class="w-[16px] h-[22px] relative">
                        <Text class="absolute left-0 top-0 text-base text-[#F4F4F5]">{notice().icon.base}</Text>
                        <Text class="absolute left-0 top-0 text-base text-[#CCFF00]">{notice().icon.accent}</Text>
                      </View>
                      <Text class="absolute text-sm font-bold text-[#F4F4F5]">{notice().letter}</Text>
                    </View>
                    <View class="flex-1 flex-col">
                      <Text class="text-base text-[#FAFAFA]">{notice().title}</Text>
                      <Text class="text-base text-[#A1A1AA]">{notice().body}</Text>
                    </View>
                    <View class="w-8 h-8 items-center justify-center" focusable onPress={() => dismiss(notice().id)}>
                      <Text class="text-base text-[#A1A1AA]">{"\uE0A8"}</Text>
                    </View>
                  </View>
                </Show>
              </View>
            )}
          </For>
          <Show when={anyNotice()}>
            <View class="flex-row justify-center">
              <View class="px-[14px] py-[8px]" focusable onPress={() => clearAll()}>
                <Text class="text-sm font-bold text-[#A1A1AA]">全部清除</Text>
              </View>
            </View>
          </Show>
        </View>
        <Show when={!anyNotice() && done() === ""}>
          <View class="absolute left-0 top-0 w-full h-full items-center justify-center">
            <Text class="text-sm text-[#A1A1AA]">没有新通知</Text>
          </View>
        </Show>
        <View class="absolute left-0 right-0 bottom-[12px] flex-row justify-center">
          <View class="w-9 h-1 rounded-sm bg-[#52525B]" />
        </View>
      </View>

      {/* The control center, pulled up from the bottom edge. */}
      <View
        class={
          screen() === 4
            ? "absolute left-0 top-0 w-full h-full bg-[#09090B] translate-y-0 opacity-100 transition-all duration-280 ease-out"
            : "absolute left-0 top-0 w-full h-full bg-[#09090B] translate-y-[360px] opacity-0 transition-all duration-280 ease-out"
        }
      >
        <View class="absolute left-0 right-0 top-[12px] flex-row justify-center">
          <View class="w-9 h-1 rounded-sm bg-[#52525B]" />
        </View>
        <Show when={control() === 0}>
          <Show when={battery() >= 0}>
            <View class="absolute left-0 right-0 top-[24px] flex-row justify-center items-center gap-2">
              <Text class="text-sm font-bold text-[#FAFAFA]">{battery()}%</Text>
              <View class="w-6 h-3 rounded-[3px] border border-[#63636B] p-[1.5px] flex-row">
                <View class="h-full rounded-[1px] bg-[#CCFF00]" style={{ width: batteryFill() }} />
              </View>
            </View>
          </Show>
          <View class="absolute left-[16px] right-[16px] top-[86px] flex-row flex-wrap justify-center gap-2">
            <For each={tiles()} by={(tile) => tile.id}>
              {(tile) => (
                <View
                  class={
                    tile().on
                      ? "w-[128px] h-14 rounded-md bg-[#FAFAFA] border border-[#FAFAFA] flex-row items-center gap-[10px] px-[14px]"
                      : "w-[128px] h-14 rounded-md border border-[#63636B] flex-row items-center gap-[10px] px-[14px]"
                  }
                  focusable
                  onPress={() => toggle(tile().id)}
                >
                  <Text class={tile().on ? "text-lg font-bold text-[#09090B]" : "text-lg font-bold text-[#FAFAFA]"}>{tile().glyph}</Text>
                  <View class="flex-col">
                    <Text class={tile().on ? "text-base text-[#09090B]" : "text-base text-[#FAFAFA]"}>{tile().label}</Text>
                    <Text class={tile().on ? "text-sm text-[#3F3F46]" : "text-sm text-[#A1A1AA]"}>{tile().detail}</Text>
                  </View>
                </View>
              )}
            </For>
          </View>
        </Show>
        <Show when={control() === 1}>
          <View class="absolute left-0 top-0 w-full h-full flex-col items-center justify-center gap-[10px]">
            <Text class="text-lg font-bold text-[#FAFAFA]">亮度</Text>
            <View class="w-[64px] h-[160px] rounded-lg border border-[#63636B] overflow-hidden relative">
              <View class="absolute left-0 bottom-0 w-[62px] bg-[#FAFAFA]" style={{ height: brightnessFill() }} />
              <View class="absolute left-0 top-0 w-[62px] h-[158px] flex-col">
                <View class="w-[62px] h-[32px]" focusable onPress={() => setBrightness(100)} />
                <View class="w-[62px] h-[32px]" focusable onPress={() => setBrightness(80)} />
                <View class="w-[62px] h-[32px]" focusable onPress={() => setBrightness(60)} />
                <View class="w-[62px] h-[31px]" focusable onPress={() => setBrightness(40)} />
                <View class="w-[62px] h-[31px]" focusable onPress={() => setBrightness(20)} />
              </View>
              <View class="absolute left-0 right-0 bottom-[10px] flex-row justify-center">
                <Text class="text-lg font-bold text-[#09090B]">{"\uE090"}</Text>
              </View>
            </View>
            <Text class="text-sm text-[#A1A1AA]">亮度 {brightness()}%</Text>
            <View class="w-[72px] h-10 rounded-md border border-[#63636B] items-center justify-center" focusable onPress={() => closeControl()}>
              <Text class="text-sm font-bold text-[#FAFAFA]">完成</Text>
            </View>
          </View>
        </Show>
        <Show when={control() === 2}>
          <View class="absolute left-0 top-0 w-full h-full flex-col items-center justify-center gap-[10px]">
            <Text class="text-lg font-bold text-[#FAFAFA]">门户已开启</Text>
            <View class="w-[128px] h-[128px] rounded-md bg-[#FAFAFA] items-center justify-center px-2">
              <Text class="text-sm text-[#09090B] text-center">{portal()}</Text>
            </View>
            <Text class="text-sm text-[#A1A1AA]">用手机连上热点，在门户里装应用</Text>
            <View class="w-[72px] h-10 rounded-md border border-[#63636B] items-center justify-center" focusable onPress={() => closeControl()}>
              <Text class="text-sm font-bold text-[#FAFAFA]">完成</Text>
            </View>
          </View>
        </Show>
      </View>
    </View>
  );
}
