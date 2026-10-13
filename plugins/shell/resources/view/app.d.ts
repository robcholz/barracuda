// The contract between the system view and the shell Plugin: values the
// Plugin writes, and handlers it receives.
import type { Accessor } from "solid-js";
import type { i32 } from "@pocketjs/framework/solid/std";

export interface AppItem { id: i32; name: string; glyph: string; running: boolean; offered: boolean }
export interface RunItem { id: i32; name: string; glyph: string; detail: string; active: boolean }
export interface Notice { id: i32; title: string; body: string }
export interface Tile { id: i32; label: string; detail: string; on: boolean }
export interface Line { id: i32; text: string }

/** 0 home, 1 apps, 2 running, 3 notices, 4 control, 5 launching, 6 exited, 7 install, 8 app info. */
export declare const screen: Accessor<i32>;
export declare const clock: Accessor<string>;
export declare const unread: Accessor<boolean>;
export declare const headline: Accessor<string>;
export declare const hint: Accessor<string>;
export declare const alert: Accessor<string>;
export declare const alertDetail: Accessor<string>;
export declare const apps: Accessor<AppItem[]>;
export declare const running: Accessor<RunItem[]>;
export declare const notices: Accessor<Notice[]>;
export declare const tiles: Accessor<Tile[]>;
export declare const title: Accessor<string>;
export declare const glyph: Accessor<string>;
export declare const detail: Accessor<string>;
export declare const lines: Accessor<Line[]>;
export declare const primary: Accessor<string>;
export declare const secondary: Accessor<string>;

export declare function openApp(id: i32): void;
export declare function switchTo(id: i32): void;
export declare function closeApp(id: i32): void;
export declare function dismiss(id: i32): void;
export declare function clearAll(): void;
export declare function toggle(id: i32): void;
export declare function alertTap(): void;
export declare function confirm(): void;
export declare function cancel(): void;
