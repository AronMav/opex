import { test, expect } from "bun:test";
import { tgUpload } from "../drivers/telegram";

// Regression guard for the egress-proxy upload stall: the Telegram adapter must
// upload voice/photo as a FormData whose file part is a Blob (known size →
// Content-Length), NOT via grammy's chunked InputFile stream which hangs the
// HTTP proxy. See tgUpload's doc comment.

test("tgUpload posts a FormData (Blob file part) to {apiRoot}/bot{token}/{method}", async () => {
  let captured: { url: string; method?: string; body: unknown } | null = null;
  const orig = globalThis.fetch;
  globalThis.fetch = (async (url: unknown, init: unknown) => {
    const i = init as RequestInit;
    captured = { url: String(url), method: i.method, body: i.body };
    return new Response('{"ok":true}', { status: 200 });
  }) as typeof fetch;
  try {
    await tgUpload(
      "https://api.telegram.org",
      "TOK",
      "sendVoice",
      "voice",
      "voice.ogg",
      "audio/ogg",
      Buffer.from([1, 2, 3, 4, 5]),
      { chat_id: "42", caption: "hi", reply_parameters: undefined },
    );
  } finally {
    globalThis.fetch = orig;
  }

  expect(captured).not.toBeNull();
  expect(captured!.url).toBe("https://api.telegram.org/botTOK/sendVoice");
  expect(captured!.method).toBe("POST");
  const form = captured!.body as FormData;
  expect(form).toBeInstanceOf(FormData);
  expect(form.get("chat_id")).toBe("42");
  expect(form.get("caption")).toBe("hi");
  // undefined fields are skipped (no chunked-forcing empty parts)
  expect(form.has("reply_parameters")).toBe(false);
  const file = form.get("voice");
  expect(file).toBeInstanceOf(Blob); // Blob ⇒ known size ⇒ Content-Length
  expect((file as Blob).size).toBe(5);
});

test("tgUpload throws on a non-ok Telegram response", async () => {
  const orig = globalThis.fetch;
  globalThis.fetch = (async () =>
    new Response("Bad Request: chat not found", { status: 400 })) as typeof fetch;
  let threw: Error | null = null;
  try {
    await tgUpload(
      "https://api.telegram.org",
      "TOK",
      "sendVoice",
      "voice",
      "v.ogg",
      "audio/ogg",
      Buffer.from([1]),
      { chat_id: "1" },
    );
  } catch (e) {
    threw = e as Error;
  } finally {
    globalThis.fetch = orig;
  }
  expect(threw).not.toBeNull();
  expect(threw!.message).toContain("sendVoice failed (400)");
});
