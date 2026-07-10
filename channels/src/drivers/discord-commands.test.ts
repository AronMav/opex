import { describe, it, expect } from "bun:test";
import { commandsToDiscord, reconstructCommandText } from "./discord-commands";

describe("commandsToDiscord", () => {
  it("maps a command with a choice arg to a String option with choices", () => {
    const out = commandsToDiscord([{
      name: "summarize_video", description: "Summarize a video",
      args: [{ name: "source", arg_type: "string", required: false },
             { name: "summary_length", arg_type: "string", required: false,
               choices: { kind: "static", values: [{value:"short",label:"short"},{value:"long",label:"long"}] } }],
    }]);
    expect(out).toEqual([{
      name: "summarize_video", description: "Summarize a video",
      options: [
        { type: 3, name: "source", description: "source", required: false },
        { type: 3, name: "summary_length", description: "summary_length", required: false,
          choices: [{ name: "short", value: "short" }, { name: "long", value: "long" }] },
      ],
    }]);
  });

  it("drops invalid names and clamps empty description to the name", () => {
    const out = commandsToDiscord([
      { name: "Bad Name", description: "x" },
      { name: "ok", description: "" },
    ]);
    expect(out).toEqual([{ name: "ok", description: "ok" }]);
  });
});

describe("reconstructCommandText", () => {
  it("joins name + non-empty values", () => {
    expect(reconstructCommandText("summarize_video", { source: "https://x/y", summary_length: "long" }))
      .toBe("/summarize_video https://x/y long");
  });
  it("bare command when no values", () => {
    expect(reconstructCommandText("status", {})).toBe("/status");
  });
});
