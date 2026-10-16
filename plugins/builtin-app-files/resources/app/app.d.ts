// The contract between the view and the app's Lua code.
import type { Accessor } from "solid-js";
import type { i32 } from "@pocketjs/framework/solid/std";

export interface Area { id: i32; name: string; count: string; glyph: string }
export interface File { id: i32; path: string; size: string }

/** 0 the areas, 1 one area's files. */
export declare const page: Accessor<i32>;
export declare const clock: Accessor<string>;
export declare const areas: Accessor<Area[]>;
export declare const area: Accessor<string>;
export declare const summary: Accessor<string>;
export declare const files: Accessor<File[]>;
export declare const empty: Accessor<boolean>;

export declare function open(id: i32): void;
