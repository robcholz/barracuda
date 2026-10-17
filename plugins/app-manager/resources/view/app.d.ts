// The contract between the system view and the system UI: values the
// Plugin writes, and handlers it receives.
import type { Accessor } from "solid-js";
import type { i32 } from "@pocketjs/framework/solid/std";

/** An icon's glyph layers in the system icon font; "" where it has none. */
export interface Icon { base: string; accent: string; dim: string }
export interface AppItem { id: i32; name: string; icon: Icon; letter: string; running: boolean; offered: boolean; x: i32; y: i32 }
export interface RunItem { id: i32; name: string; icon: Icon; letter: string; detail: string; active: boolean }
export interface Notice { id: i32; title: string; body: string; icon: Icon; letter: string; confirm: boolean }
export interface Tile { id: i32; label: string; detail: string; glyph: string; on: boolean }
export interface Line { id: i32; text: string; glyph: string }
/** One launcher page; `on` is the page on the display. */
export interface Dot { id: i32; on: boolean }

/**
 * 0 home, 1 apps, 2 running, 3 notices, 4 control, 5 launching, 6 exited,
 * 7 install, 8 app info, 9 app menu.
 */
export declare const screen: Accessor<i32>;

// Status bar.
export declare const clock: Accessor<string>;
export declare const unread: Accessor<boolean>;
export declare const quiet: Accessor<boolean>;
export declare const offline: Accessor<boolean>;
export declare const battery: Accessor<i32>;
/** The battery gauge's filled width in pixels. */
export declare const batteryFill: Accessor<i32>;

// Home: 0 standby, 1 listening, 2 thinking, 3 answering.
export declare const agent: Accessor<i32>;
export declare const question: Accessor<string>;
export declare const answer: Accessor<string>;
export declare const card: Accessor<string>;
export declare const cardLabel: Accessor<string>;
export declare const cardGlyph: Accessor<string>;
/** 0 neutral, 1 warning, 2 danger. */
export declare const cardTone: Accessor<i32>;

export declare const apps: Accessor<AppItem[]>;
/** The launcher's pages when it has more than one; swipe up and down to turn. */
export declare const dots: Accessor<Dot[]>;
export declare const running: Accessor<RunItem[]>;
export declare const anyRunning: Accessor<boolean>;
export declare const notices: Accessor<Notice[]>;
export declare const anyNotice: Accessor<boolean>;
export declare const done: Accessor<string>;
export declare const tiles: Accessor<Tile[]>;
/** 0 tiles, 1 brightness, 2 portal. */
export declare const control: Accessor<i32>;
export declare const brightness: Accessor<i32>;
/** The brightness bar's filled height in pixels. */
export declare const brightnessFill: Accessor<i32>;
export declare const portal: Accessor<string>;

// The app a launching, exited, install, info, or menu screen is about.
export declare const title: Accessor<string>;
export declare const detail: Accessor<string>;
export declare const icon: Accessor<Icon>;
export declare const letter: Accessor<string>;
export declare const lines: Accessor<Line[]>;
export declare const primary: Accessor<string>;
export declare const secondary: Accessor<string>;
export declare const removable: Accessor<boolean>;
/** The app cannot run on this device: the install button is shown disabled. */
export declare const blocked: Accessor<boolean>;

export declare function openApp(id: i32): void;
export declare function switchTo(id: i32): void;
export declare function closeApp(id: i32): void;
export declare function dismiss(id: i32): void;
export declare function allow(id: i32): void;
export declare function deny(id: i32): void;
export declare function clearAll(): void;
export declare function toggle(id: i32): void;
export declare function setBrightness(level: i32): void;
export declare function closeControl(): void;
export declare function cardTap(): void;
export declare function confirm(): void;
export declare function cancel(): void;
export declare function info(): void;
