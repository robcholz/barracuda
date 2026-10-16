import { Text, View } from "@pocketjs/framework/solid/components";
import { before, good, reading, verdict } from "./app";

export default function Air() {
  return (
    <View class="w-full h-full flex-col items-center justify-center bg-[#09090B]">
      <Text class="text-sm text-[#A1A1AA]">室内 PM2.5</Text>
      <Text class="text-5xl font-bold text-[#FAFAFA] scale-150 my-[22px]">{reading()}</Text>
      <View class="flex-row items-center gap-2">
        <View class={good() ? "w-[8px] h-[8px] rounded-full bg-[#CCFF00]" : "w-[8px] h-[8px] rounded-full bg-[#ED8E04]"} />
        <Text class="text-lg text-[#FAFAFA]">{verdict()}</Text>
      </View>
      <Text class="mt-[14px] text-sm text-[#A1A1AA]">{before()}</Text>
    </View>
  );
}
