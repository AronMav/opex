// ui/src/types/ws.ts — thin re-export over the ts-rs codegen (see crates/opex-types/src/ws.rs).
// Do NOT hand-edit shapes here; fix consumers instead. Regenerate the source with `make gen-types`.

export type { WsEvent } from "./ws.generated";
import type { WsEvent } from "./ws.generated";

export type WsEventType = WsEvent["type"];
export type WsEventOf<T extends WsEventType> = Extract<WsEvent, { type: T }>;

// WsLog is the only alias with a live consumer; the other historical back-compat
// aliases were removed (no importers). Name any other event shape inline with
// `WsEventOf<"event_type">` instead of adding a dead alias.
export type WsLog = WsEventOf<"log">;
