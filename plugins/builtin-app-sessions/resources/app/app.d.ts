// The contract between the view and the app's Lua code.
import type { Accessor } from "solid-js";
import type { i32 } from "@pocketjs/framework/solid/std";

export interface Session { id: i32; title: string; detail: string }
/** One message; `mine` is the person's. */
export interface Said { id: i32; mine: boolean; text: string }

/** 0 the conversations, 1 one conversation. */
export declare const page: Accessor<i32>;
export declare const clock: Accessor<string>;
export declare const sessions: Accessor<Session[]>;
export declare const empty: Accessor<boolean>;

// The open conversation: its title, its latest messages oldest first, and
// whether the Agent is still answering in it.
export declare const title: Accessor<string>;
export declare const messages: Accessor<Said[]>;
export declare const thinking: Accessor<boolean>;

export declare function open(id: i32): void;
