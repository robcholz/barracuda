// The contract between the view and the app's Lua code.
import type { Accessor } from "solid-js";
import type { i32 } from "@pocketjs/framework/solid/std";

/** An icon's glyph layers in the system icon font; "" where it has none. */
export interface Icon { base: string; accent: string; dim: string }
/** A row of the first page, or of the storage or about pages. */
export interface Row { id: i32; label: string; value: string; glyph: string; mono: boolean }
export interface Network { id: i32; ssid: string; signal: Icon; locked: boolean }
export interface App { id: i32; name: string; icon: Icon; letter: string; state: string; live: boolean }

/** 0 settings, 1 Wi-Fi, 2 sound and display, 3 apps, 4 storage, 5 about. */
export declare const page: Accessor<i32>;
export declare const clock: Accessor<string>;
export declare const rows: Accessor<Row[]>;

// Wi-Fi.
export declare const ssid: Accessor<string>;
export declare const connected: Accessor<boolean>;
/** Whether `ssid` is the network's name, not a word for the state. */
export declare const named: Accessor<boolean>;
export declare const network: Accessor<string>;
export declare const networks: Accessor<Network[]>;
export declare const scanning: Accessor<boolean>;

// Sound and display: fills are the bars' widths in pixels.
export declare const volume: Accessor<i32>;
export declare const volumeFill: Accessor<i32>;
export declare const brightness: Accessor<i32>;
export declare const brightnessFill: Accessor<i32>;
export declare const timeout: Accessor<string>;
export declare const muted: Accessor<boolean>;

export declare const apps: Accessor<App[]>;

// Storage: the bar's parts in pixels.
export declare const used: Accessor<string>;
export declare const usedNote: Accessor<string>;
export declare const mediaFill: Accessor<i32>;
export declare const appsFill: Accessor<i32>;
export declare const cacheFill: Accessor<i32>;

// About.
export declare const board: Accessor<string>;

export declare function open(id: i32): void;
export declare function setVolume(level: i32): void;
export declare function setBrightness(level: i32): void;
export declare function nextTimeout(): void;
export declare function toggleMute(): void;
