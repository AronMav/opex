/**
 * Email (IMAP/SMTP) channel driver.
 * Allows the agent to monitor an inbox and reply to emails.
 */

import { ImapFlow } from "imapflow";
import nodemailer from "nodemailer";
import { simpleParser } from "mailparser";
import type { BridgeHandle, OutboundAction } from "../bridge";
import type { ChannelDriver } from "../session";
import type { IncomingMessageDto } from "../types";
import { getStrings } from "../localization";
import { parseDirectives } from "./common";

type ParsedMail = Awaited<ReturnType<typeof simpleParser>>;

/** Organizational-ish domain of an email address (everything after the last @). */
function fromDomain(addr: string): string {
  const at = addr.lastIndexOf("@");
  return at >= 0 ? addr.slice(at + 1).toLowerCase() : "";
}

/**
 * F102: the From header is attacker-controlled, so it must NOT be trusted for
 * access/owner decisions on its own — a spoofed `From: owner@domain` would
 * otherwise grant owner control.
 *
 * CRITICAL: an attacker can also inject their OWN `Authentication-Results:
 * ...dmarc=pass` line into the message, so we must NOT scan every AR header. Per
 * RFC 8601, only the AR header(s) STAMPED BY OUR OWN RECEIVING MTA are
 * trustworthy — identified by matching `authserv-id` (the token before the first
 * `;`) against the operator-configured `authserv_id`. The receiving MTA prepends
 * its header, so the FIRST AR line whose authserv-id matches is authoritative;
 * we evaluate only that one. Without a configured `authserv_id` — or with no
 * matching AR header — we fail CLOSED (untrusted).
 */
function isFromAuthenticated(
  parsed: ParsedMail,
  fromAddress: string,
  trustedAuthservId: string | undefined,
): boolean {
  if (!trustedAuthservId) return false; // cannot verify without a trusted receiver id
  const trusted = trustedAuthservId.toLowerCase();

  // headerLines preserves each raw header in received order (MTA prepends its AR).
  const arLines = (parsed.headerLines ?? [])
    .filter((h) => h.key === "authentication-results")
    .map((h) => h.line.replace(/^authentication-results:\s*/i, ""));

  for (const val of arLines) {
    const authservId = val.split(";")[0].trim().toLowerCase().split(/\s+/)[0];
    if (authservId !== trusted) continue; // not stamped by OUR receiver — ignore
    // First AR from the trusted receiver is authoritative (its verdict).
    const lower = val.toLowerCase();
    if (/\bdmarc=pass\b/.test(lower)) return true;
    if (/\bdkim=pass\b/.test(lower)) {
      const dom = fromDomain(fromAddress);
      const m = lower.match(/dkim=pass[^;]*?header\.d=([a-z0-9.\-]+)/);
      const d = m?.[1] ?? "";
      if (d && (d === dom || dom.endsWith("." + d) || d.endsWith("." + dom))) return true;
    }
    return false; // trusted receiver did not attest a pass
  }
  return false; // no AR header from the trusted receiver
}

export function createEmailDriver(
  bridge: BridgeHandle,
  credential: string,
  channelConfig: Record<string, unknown> | undefined,
  language: string,
  _typingMode: string,
): ChannelDriver {
  const strings = getStrings(language);
  const password = credential;
  // F102: operator-configured authserv-id of OUR receiving MTA — the only
  // Authentication-Results identity we trust (defaults to the imap host). Without
  // it, sender authentication fails closed.
  const trustedAuthservId =
    (channelConfig?.authserv_id as string) || (channelConfig?.imap_host as string) || "";

  const imapConfig = {
    host: (channelConfig?.imap_host as string) || "imap.gmail.com",
    port: (channelConfig?.imap_port as number) || 993,
    secure: true,
    auth: {
      user: (channelConfig?.imap_user as string) || "",
      pass: password,
    },
  };

  const smtpConfig = {
    host: (channelConfig?.smtp_host as string) || "smtp.gmail.com",
    port: (channelConfig?.smtp_port as number) || 465,
    secure: true,
    auth: {
      user: (channelConfig?.smtp_user as string) || imapConfig.auth.user,
      pass: password,
    },
  };

  // F021: build a FRESH ImapFlow per connection attempt (below) rather than
  // reusing one module-scoped client. ImapFlow cannot reconnect an already-open
  // client, so a single shared instance left half-open by an error would make
  // every future connect() fail and silently kill the channel until restart.
  const makeClient = () =>
    new ImapFlow({
      host: imapConfig.host,
      port: imapConfig.port,
      secure: imapConfig.secure,
      auth: imapConfig.auth,
      logger: false,
    });
  let client: ImapFlow | null = null;

  const transporter = nodemailer.createTransport(smtpConfig);
  let stopPolling = false;

  async function sendEmail(to: string, subject: string, text: string, inReplyTo?: string) {
    await transporter.sendMail({
      from: imapConfig.auth.user,
      to,
      subject: subject.startsWith("Re:") ? subject : `Re: ${subject}`,
      text,
      inReplyTo,
      references: inReplyTo,
    });
  }

  async function pollInbox() {
    while (!stopPolling) {
      client = makeClient();
      const c = client;
      try {
        await c.connect();
        let lock = await c.getMailboxLock("INBOX");
        try {
          // Search for UNSEEN messages, then fetch each. ImapFlow returns
          // `false` when the search yields no results — narrow that out.
          const unseenUids = await c.search({ seen: false });
          if (Array.isArray(unseenUids) && unseenUids.length > 0) {
            for await (let msg of c.fetch(unseenUids, { envelope: true, source: true })) {
              // F020: isolate each message. Without this, ANY failure (agent
              // error, transient SMTP send failure) escaped before the \Seen
              // flag was set, so the same message was re-fetched every 30s
              // forever — a poison pill that also starved every message behind
              // it. Mark \Seen even on failure so a bad message can't loop.
              try {
                if (!msg.source || !msg.envelope) continue;
                const parsed = await simpleParser(msg.source);
                const from = parsed.from?.value[0]?.address || "";
                const subject = parsed.subject || "No Subject";
                const text = parsed.text || "";
                const messageId = msg.envelope.messageId;

                if (!from || !text) continue;

                // F102: reject unauthenticated (spoofable) senders before any
                // access/owner decision. Drop silently (mark \Seen, no reply) so a
                // spoofed From can't trigger backscatter to the forged address.
                if (!isFromAuthenticated(parsed, from, trustedAuthservId)) {
                  console.warn(`[email] dropping unauthenticated message from ${from} (no trusted DMARC/aligned-DKIM pass)`);
                  await c.messageFlagsAdd(msg.uid, ["\\Seen"]);
                  continue;
                }

                // Access control
                const { allowed, isOwner } = await bridge.checkAccess(from);
                if (!allowed && !isOwner) {
                  const code = await bridge.createPairingCode(from, from);
                  await sendEmail(from, subject, strings.accessRestricted(code), messageId);
                  await c.messageFlagsAdd(msg.uid, ["\\Seen"]);
                  continue;
                }

                const { text: cleanText, directives } = parseDirectives(text);
                const dto: IncomingMessageDto = {
                  user_id: from,
                  display_name: parsed.from?.value[0]?.name || from,
                  text: cleanText || text,
                  attachments: [],
                  context: {
                    subject,
                    message_id: messageId,
                    directives,
                  },
                  timestamp: new Date().toISOString(),
                };

                const { result } = bridge.sendMessage(dto);
                const response = await result;
                if (response) {
                  await sendEmail(from, subject, response, messageId);
                }

                // Mark as read
                await c.messageFlagsAdd(msg.uid, ["\\Seen"]);
              } catch (msgErr) {
                console.error("[email] message processing failed:", msgErr);
                // Best-effort: flag \Seen so a poison message isn't reprocessed
                // every poll. Errors here are swallowed (already in a failure path).
                try { await c.messageFlagsAdd(msg.uid, ["\\Seen"]); } catch {}
              }
            }
          }
        } finally {
          lock.release();
        }
      } catch (err) {
        console.error("[email] poll error:", err);
      } finally {
        // F021: ALWAYS tear the connection down, even on error, so a broken /
        // half-open client can't wedge every future poll.
        try {
          await c.logout();
        } catch {
          try { c.close(); } catch {}
        }
        if (client === c) client = null;
      }
      // Wait 30s before next poll
      await new Promise(r => setTimeout(r, 30000));
    }
  }

  return {
    start: async () => {
      pollInbox().catch(err => console.error("[email] background loop error:", err));
      console.log(`[email] monitoring ${imapConfig.auth.user}`);
    },
    stop: async () => {
      stopPolling = true;
      try { if (client) await client.logout(); } catch {}
    },
    onAction: async (action: OutboundAction) => {
      const context = action.action.context as Record<string, unknown>;
      const params = action.action.params as Record<string, unknown>;
      const to = context.from as string;
      const subject = context.subject as string;
      const messageId = context.message_id as string;

      if (action.action.action === "send_message" || action.action.action === "reply") {
        await sendEmail(to, subject, params.text as string, messageId);
      }
    },
  };
}
