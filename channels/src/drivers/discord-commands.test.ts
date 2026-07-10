import { describe, it, expect } from "bun:test";
import { commandsToDiscord, reconstructCommandText } from "./discord-commands";

describe("commandsToDiscord", () => {
  it("maps summarize_video: exposes only the source positional, excludes the menu:true choice-valve", () => {
    const out = commandsToDiscord([{
      name: "summarize_video", description: "Summarize a video",
      args: [{ name: "source", arg_type: "string", required: false },
             { name: "summary_length", arg_type: "string", required: false, menu: true,
               choices: { kind: "static", values: [{value:"short",label:"short"},{value:"long",label:"long"}] } }],
    }]);
    expect(out).toEqual([{
      name: "summarize_video", description: "Summarize a video",
      options: [
        { type: 3, name: "source", description: "source", required: false },
      ],
    }]);
  });

  it("a command whose only args are menu:true valves produces no options key", () => {
    const out = commandsToDiscord([{
      name: "only_menu", description: "Only a menu valve",
      args: [{ name: "summary_length", arg_type: "string", required: false, menu: true,
               choices: { kind: "static", values: [{value:"short",label:"short"}] } }],
    }]);
    expect(out).toEqual([{ name: "only_menu", description: "Only a menu valve" }]);
  });

  it("maps a non-menu choice arg to a String option with choices", () => {
    const out = commandsToDiscord([{
      name: "pick", description: "Pick one",
      args: [{ name: "choice", arg_type: "string", required: false,
               choices: { kind: "static", values: [{value:"short",label:"short"},{value:"long",label:"long"}] } }],
    }]);
    expect(out).toEqual([{
      name: "pick", description: "Pick one",
      options: [
        { type: 3, name: "choice", description: "choice", required: false,
          choices: [{ name: "short", value: "short" }, { name: "long", value: "long" }] },
      ],
    }]);
  });

  it("clamps choice name/value to 100 chars", () => {
    const long = "x".repeat(150);
    const out = commandsToDiscord([{
      name: "pick", description: "Pick one",
      args: [{ name: "choice", arg_type: "string", required: false,
               choices: { kind: "static", values: [{ value: long, label: long }] } }],
    }]);
    const choices = out[0]!.options![0]!.choices!;
    expect(choices[0]!.name.length).toBe(100);
    expect(choices[0]!.value.length).toBe(100);
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
  it("joins name + non-empty values (source only, since menu valves aren't collected inline)", () => {
    expect(reconstructCommandText("summarize_video", { source: "https://x/y" }))
      .toBe("/summarize_video https://x/y");
  });
  it("bare command when no values", () => {
    expect(reconstructCommandText("status", {})).toBe("/status");
  });
});
