import { Text, View } from "@pocketjs/framework/solid/components";
import { confirm } from "./app";

// A view written for any display: it fills the screen and centers its card.
// `layout-baked` asks the build to solve it for the display it targets.
export default function App() {
  return (
    <View class="w-full h-full layout-baked flex-col items-center justify-center gap-3 bg-[#09090B]">
      <Text class="text-2xl font-bold text-[#FAFAFA]">Barracuda</Text>
      <View class="flex-row gap-2">
        <View class="w-[64px] h-[36px] rounded-md bg-[#27272A]" />
        <View class="px-4 py-2 rounded-md bg-[#CCFF00]" focusable onPress={() => confirm()}>
          <Text class="text-[#09090B]">OK</Text>
        </View>
      </View>
    </View>
  );
}
