import { Text, View } from "@pocketjs/framework/solid/components";
import { label, message, value } from "./app";

// A paragraph centered in a padded column, and a row whose label takes the
// room its value leaves.
export default function App() {
  return (
    <View class="w-full h-full flex-col items-center justify-center gap-3 px-[20px] bg-[#09090B]">
      <Text class="text-center text-[#FAFAFA]">{message()}</Text>
      <View class="w-full flex-row items-center gap-2">
        <Text class="flex-1 min-w-0 text-[#FAFAFA]">{label()}</Text>
        <Text class="text-sm text-[#A1A1AA]">{value()}</Text>
      </View>
    </View>
  );
}
