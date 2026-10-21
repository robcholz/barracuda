import { Text, View } from "@pocketjs/framework/solid/components";
import { before, good, reading, verdict } from "./app";

export default function Air() {
  return (
    <View class="w-full h-full flex-col items-center px-[16px] overflow-hidden bg-[#09090B]">
      <View class="flex-1 min-h-0" />
      <Text class="shrink-0 text-sm text-[#A1A1AA]">Indoor PM2.5</Text>
      <Text class="shrink-0 text-5xl font-bold text-[#FAFAFA] scale-150 my-[22px]">{reading()}</Text>
      <View class="shrink-0 flex-row items-center gap-2">
        <View class={good() ? "w-[8px] h-[8px] rounded-full bg-[#CCFF00]" : "w-[8px] h-[8px] rounded-full bg-[#ED8E04]"} />
        <Text class="text-lg text-[#FAFAFA]">{verdict()}</Text>
      </View>
      <Text class="shrink-0 mt-[14px] text-sm text-[#A1A1AA] text-center">{before()}</Text>
      <View class="flex-1 min-h-0" />
    </View>
  );
}
