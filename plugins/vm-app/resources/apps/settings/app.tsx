import { For, Text, View } from "@pocketjs/framework/solid/components";
import { next, page, pages, rows, section } from "./app";

export default function Settings() {
  return (
    <View class="w-full h-full flex-col items-center justify-center gap-2 px-8 bg-[#09090B]">
      <Text class="text-base text-[#FAFAFA]">设置</Text>
      <View class="w-full flex-col gap-1">
        <Text class="text-base text-[#A1A1AA] pb-1">{section()}</Text>
        <For each={rows()} by={(row) => row.id}>
          {(row) => (
            <View class="w-full flex-row items-center justify-between px-3 py-2 mb-1 rounded-md border border-[#27272A]">
              <Text class="text-base text-[#FAFAFA]">{row().label}</Text>
              <Text class="text-base text-[#A1A1AA]">{row().value}</Text>
            </View>
          )}
        </For>
      </View>
      <View class="px-4 py-2 rounded-md border border-[#63636B] focus:bg-[#18181B]" focusable onPress={() => next()}>
        <Text class="text-base text-[#FAFAFA]">{page()} / {pages()}</Text>
      </View>
    </View>
  );
}
