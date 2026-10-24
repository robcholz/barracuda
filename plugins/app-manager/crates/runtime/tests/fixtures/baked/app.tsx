import { Text, View } from "@pocketjs/framework/solid/components";
import { confirm } from "./app";

// A view written for any display: it fills the screen and centers its card,
// whose paragraph wraps to the display's width.
// `layout-baked` asks the build to solve it for the display it targets.
export default function App() {
  return (
    <View class="w-full h-full layout-baked flex-col items-center justify-center gap-3 px-[24px] bg-[#09090B]">
      <Text class="text-2xl font-bold text-[#FAFAFA]">Barracuda</Text>
      <Text class="text-center text-[#A1A1AA]">Hold the Agent key and ask anything, or press OK to carry on</Text>
      <View class="flex-row gap-2">
        <View class="w-[64px] h-[36px] rounded-md bg-[#27272A]" />
        <View class="px-4 py-2 rounded-md bg-[#CCFF00]" focusable onPress={() => confirm()}>
          <Text class="text-[#09090B]">OK</Text>
        </View>
      </View>
    </View>
  );
}
