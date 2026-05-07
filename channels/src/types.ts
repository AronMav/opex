// channels/src/types.ts
//
// Hand-coded thin wrapper. Re-exports types from the auto-generated
// types.generated.ts so existing imports `from "./types"` continue
// to resolve unchanged.
//
// Source of truth: crates/hydeclaw-types/src/lib.rs (registered for
// codegen in crates/hydeclaw-core/src/dto_export/channels_ts.rs).

export type {
  ChannelActionDto,
  ChannelInbound,
  ChannelOutbound,
  IncomingMessageDto,
  MediaAttachment,
  MediaType,
} from "./types.generated";

// TEMP — deleted in T4.5 commit (zero consumers verified via grep).
export const PHASES = {
  THINKING: "thinking",
  CALLING_TOOL: "calling_tool",
  COMPOSING: "composing",
} as const;

export const CHANNEL_TYPES = ["telegram", "discord", "matrix", "irc", "slack", "whatsapp"] as const;
export type ChannelType = (typeof CHANNEL_TYPES)[number];
