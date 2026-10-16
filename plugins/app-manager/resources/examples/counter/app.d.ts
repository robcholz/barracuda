// The contract between the view and the app's Lua code: values Lua writes
// with `view:set`, and handlers Lua receives from `view:next`.
import type { Accessor } from "solid-js";
import type { i32 } from "@pocketjs/framework/solid/std";
export interface Entry { id: i32; label: string }
export declare const count: Accessor<i32>;
export declare const history: Accessor<Entry[]>;
export declare function increment(by: i32): void;
export declare function reset(): void;
