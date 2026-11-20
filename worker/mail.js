// The mail extension's Email Worker: Cloudflare's Email Routing hands it each mail sent to a claimed address. It
// keeps the sender, the recipient, the subject, the text (the HTML as text when there's no text part) and the
// Authentication-Results header, drops attachments (their names are kept), cuts the text at MAX_CHARS, and stores
// it as JSON in Workers KV under mail/<local part>/<time>-<id>, where the extension collects it. An address takes at
// most DAILY mails a UTC day; the rest are refused. It never replies or forwards.
//
// KV's free tier writes 1,000 keys a day, so each mail costs three: the mail, its address's count for the day, and
// mail/latest (the newest mail's key), which is the one key the extension reads every 30 s instead of listing.

import { latin1, parse } from "./parse.js";

const DAY_SECONDS = 86400;

function number(value, fallback) {
  const n = parseInt(value, 10);
  return Number.isFinite(n) && n > 0 ? n : fallback;
}

function id() {
  const b = new Uint8Array(6);
  crypto.getRandomValues(b);
  return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
}

export default {
  async email(message, env) {
    const daily = number(env.DAILY, 50);
    const most = number(env.MAX_CHARS, 20000);
    const to = String(message.to).toLowerCase();
    const local = to.split("@")[0];
    const now = new Date();
    const countKey = `count/${local}/${now.toISOString().slice(0, 10)}`;
    const count = number(await env.MAIL.get(countKey), 0);
    if (count >= daily) {
      message.setReject(`${to} takes at most ${daily} mails a day`);
      return;
    }
    const raw = latin1(new Uint8Array(await new Response(message.raw).arrayBuffer()));
    const mail = parse(raw);
    const chars = Array.from(mail.text);
    const cut = Math.max(0, chars.length - most);
    const stored = {
      from: String(message.from),
      to,
      subject: mail.subject,
      date: mail.date,
      message_id: mail.messageId,
      auth: mail.auth || message.headers.get("authentication-results") || "",
      text: cut ? chars.slice(0, most).join("") : mail.text,
      cut,
      attachments: mail.attachments,
      size: message.rawSize,
      received: now.toISOString(),
    };
    const key = `mail/${local}/${String(now.getTime()).padStart(13, "0")}-${id()}`;
    await env.MAIL.put(key, JSON.stringify(stored));
    await env.MAIL.put(countKey, String(count + 1), { expirationTtl: 2 * DAY_SECONDS });
    try {
      // one write a second to a key: a mail arriving in the same second still gets collected by the next full list
      await env.MAIL.put("mail/latest", key);
    } catch {}
  },
};
