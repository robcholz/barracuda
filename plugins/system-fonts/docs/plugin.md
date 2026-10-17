# System Fonts Plugin

Ships the system fonts: the font atlases every view on a display draws with
for the font slots it ships none for.

- Plugin ID: `system-fonts`
- Dependencies: `app-manager`
- Settings: none
- Context resources: none
- Provided capabilities: none
- Required capabilities: `SystemFonts` from `app-manager`
- Workflow: none
- Agent Tools: none
- Lua packages: none
- Web: none
- Tasks: none
- Storage: the fonts in `/resources/fonts`

At registration the Plugin hands its filesystem to `app-manager` with
`SystemFonts::read_from`, and the UI kit reads `/resources/fonts` from it
before the first view opens. The fonts are about 2.4 MB, so they ship apart
from `app-manager`: a Board with a display enables this Plugin, and one
without leaves it out and carries none. Without it, views draw only with the
atlases they ship, and text in other slots shows placeholder boxes.

## The fonts

`resources/fonts/build.ts` bakes them from Noto Sans SC: ASCII and every
character in the sources `resources/fonts/sources.txt` names (the
`app-manager` Plugin's system view, system UI, app kit, and examples) and in
`charset.txt`, at the sizes the system view and the system apps use, and the
system icon font's glyphs at the sizes icons are drawn at. The 16 px regular
slot is the prose slot: it also holds GB 2312's 3,755 common characters, so
text no source names, such as an Agent's answer, a notice, or a
conversation's title, is drawn in it. Text with characters outside an atlas
shows a placeholder box.

```sh
POCKETJS_ROOT=~/src/pocketjs CJK_FONT_DIR=~/fonts cargo plugin run fonts --plugin system-fonts
```

`CJK_FONT_DIR` holds `NotoSansSC-Regular.otf` and `NotoSansSC-Bold.otf`
(github.com/notofonts/noto-cjk, `Sans/SubsetOTF/SC`). Rebuild after adding
text with new characters to those sources or to a system app's Lua.
