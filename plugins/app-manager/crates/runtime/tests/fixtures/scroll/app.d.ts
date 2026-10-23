// The contract between the view and the app's Lua code.
import type { Accessor } from "solid-js";
import type { i32 } from "@pocketjs/framework/solid/std";

export interface Row { id: i32; name: string }

export declare const rows: Accessor<Row[]>;
/** Keep the rows at the bottom, as a chat does. */
export declare const end: Accessor<boolean>;
/** Picks row `id`. */
export declare function pick(id: i32): void;
