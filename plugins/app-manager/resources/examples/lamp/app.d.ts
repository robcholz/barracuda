// The contract between the view and the app's Lua code.
import type { Accessor } from "solid-js";

export declare const on: Accessor<boolean>;
export declare const warm: Accessor<boolean>;
export declare const status: Accessor<string>;
export declare function power(): void;
export declare function setWarm(): void;
export declare function setCool(): void;
