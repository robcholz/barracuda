// The contract between the view and the app's Lua code.
import type { Accessor } from "solid-js";

export declare const finder: Accessor<string>;
export declare const busy: Accessor<boolean>;
export declare function shoot(): void;
