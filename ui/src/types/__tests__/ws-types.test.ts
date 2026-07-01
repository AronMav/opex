import { describe, it, expect } from "vitest";
import type { WsEvent, WsEventOf, WsFileJobProgress } from "@/types/ws";

describe("WsEvent union includes file_job_progress", () => {
  it("WsFileJobProgress type is assignable to WsEvent", () => {
    const event: WsFileJobProgress = {
      type: "file_job_progress",
      job_id: "j1",
      handler_id: "transcribe",
      session_id: "s1",
      phase: "processing",
      pct: 50,
      status: "running",
    };
    // Type-level check: WsEvent must accept file_job_progress
    const asUnion: WsEvent = event;
    expect(asUnion.type).toBe("file_job_progress");
  });

  it("WsEventOf<'file_job_progress'> extracts the correct shape", () => {
    type Extracted = WsEventOf<"file_job_progress">;
    const sample: Extracted = {
      type: "file_job_progress",
      job_id: "j2",
      handler_id: "h",
      session_id: "s2",
      phase: "done",
      pct: 100,
      status: "done",
    };
    expect(sample.job_id).toBe("j2");
    expect(sample.status).toBe("done");
  });
});
