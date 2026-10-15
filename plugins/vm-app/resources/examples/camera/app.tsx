import { Text, View } from "@pocketjs/framework/solid/components";
import { busy, finder, shoot } from "./app";

export default function Camera() {
  return (
    <View class="w-full h-full relative bg-[#09090B]">
      <View class="absolute left-[69px] top-[64px] w-[222px] h-[130px] rounded-lg bg-[#18181B] border border-[#27272A] items-center justify-center">
        <Text class="text-base text-[#A1A1AA]">{finder()}</Text>
      </View>
      <View class="absolute left-[150px] top-[206px] w-[60px] h-[60px] rounded-full border-[3px] border-[#FAFAFA] items-center justify-center" focusable onPress={() => shoot()}>
        <View class={busy() ? "w-[46px] h-[46px] rounded-full bg-[#A1A1AA]" : "w-[46px] h-[46px] rounded-full bg-[#FAFAFA]"} />
      </View>
    </View>
  );
}
