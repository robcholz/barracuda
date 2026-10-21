import { Text, View } from "@pocketjs/framework/solid/components";
import { on, power, setCool, setWarm, status, warm } from "./app";

export default function Lamp() {
  return (
    <View class="w-full h-full flex-col items-center justify-center bg-[#09090B]">
      <View
        class={
          on()
            ? "w-[92px] h-[92px] rounded-full bg-[#FAFAFA] items-center justify-center"
            : "w-[92px] h-[92px] rounded-full bg-[#27272A] border border-[#3F3F46] items-center justify-center"
        }
        focusable
        onPress={() => power()}
      >
        <Text class={on() ? "text-4xl text-[#09090B]" : "text-4xl text-[#FAFAFA]"}>{""}</Text>
      </View>
      <Text class="mt-[16px] text-lg text-[#FAFAFA]">{status()}</Text>
      <View class="mt-[12px] w-[200px] h-[46px] rounded-md bg-[#18181B] border border-[#27272A] flex-row items-center p-[3px]">
        <View
          class={warm() ? "flex-1 h-[38px] rounded bg-[#27272A] border border-[#3F3F46] items-center justify-center" : "flex-1 h-[38px] items-center justify-center"}
          focusable
          onPress={() => setWarm()}
        >
          <Text class="text-base text-[#FAFAFA]">Warm</Text>
        </View>
        <View
          class={warm() ? "flex-1 h-[38px] items-center justify-center" : "flex-1 h-[38px] rounded bg-[#27272A] border border-[#3F3F46] items-center justify-center"}
          focusable
          onPress={() => setCool()}
        >
          <Text class="text-base text-[#FAFAFA]">Cool</Text>
        </View>
      </View>
    </View>
  );
}
