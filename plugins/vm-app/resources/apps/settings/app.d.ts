// The contract between the view and the app's Lua code.
import type { Accessor } from "solid-js";
import type { i32 } from "@pocketjs/framework/solid/std";
export interface Row { id: i32; label: string; value: string }
export declare const section: Accessor<string>;
export declare const rows: Accessor<Row[]>;
export declare const page: Accessor<i32>;
export declare const pages: Accessor<i32>;
export declare function next(): void;
