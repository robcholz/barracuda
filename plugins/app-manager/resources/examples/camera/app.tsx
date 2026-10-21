import { Text, View } from "@pocketjs/framework/solid/components";
import { busy, finder, shoot } from "./app";

// The viewfinder takes the room the shutter leaves, up to the size the design
// draws it at; both stay centered on any display.
export default function Camera() {
  return (
    <View class="w-full h-full flex-col items-center px-[16px] overflow-hidden bg-[#09090B]">
      <View class="flex-1 min-h-0" />
      <View class="w-full max-w-[222px] h-[130px] min-h-[48px] rounded-lg bg-[#18181B] border border-[#27272A] items-center justify-center px-2">
        <Text class="text-base text-[#A1A1AA] text-center">{finder()}</Text>
      </View>
      <View class="shrink-0 mt-[12px] w-[60px] h-[60px] rounded-full border-[3px] border-[#FAFAFA] items-center justify-center" focusable onPress={() => shoot()}>
        <View class={busy() ? "w-[46px] h-[46px] rounded-full bg-[#A1A1AA]" : "w-[46px] h-[46px] rounded-full bg-[#FAFAFA]"} />
      </View>
      <View class="flex-1 min-h-0" />
    </View>
  );
}
