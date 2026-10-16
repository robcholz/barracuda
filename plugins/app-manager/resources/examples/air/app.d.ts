// The contract between the view and the app's Lua code.
import type { Accessor } from "solid-js";

export declare const reading: Accessor<string>;
export declare const verdict: Accessor<string>;
export declare const good: Accessor<boolean>;
export declare const before: Accessor<string>;
