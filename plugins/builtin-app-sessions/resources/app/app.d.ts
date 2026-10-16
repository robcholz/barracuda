// The contract between the view and the app's Lua code.
import type { Accessor } from "solid-js";
import type { i32 } from "@pocketjs/framework/solid/std";

export interface Session { id: i32; title: string; detail: string }

/** 0 the conversations, 1 one conversation. */
export declare const page: Accessor<i32>;
export declare const clock: Accessor<string>;
export declare const sessions: Accessor<Session[]>;
export declare const empty: Accessor<boolean>;

// The open conversation: what the person first asked, and when it was last
// used.
export declare const asked: Accessor<string>;
export declare const when: Accessor<string>;
// What the Agent last said on this device, and whether it is thinking.
export declare const said: Accessor<string>;
export declare const thinking: Accessor<boolean>;

export declare function open(id: i32): void;
