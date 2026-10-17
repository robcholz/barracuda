// The contract between the view and the app's Lua code.
import type { Accessor } from "solid-js";
import type { i32 } from "@pocketjs/framework/solid/std";

export interface Flow { id: i32; name: string; detail: string; glyph: string; on: boolean }

export declare const clock: Accessor<string>;
export declare const flows: Accessor<Flow[]>;
export declare const empty: Accessor<boolean>;
/** Turns Workflow row `id` on or off. */
export declare function toggle(id: i32): void;
