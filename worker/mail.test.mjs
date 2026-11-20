// The Worker under Node's test runner, with a fake KV: `node --test worker/mail.test.mjs`.
import assert from "node:assert/strict";
import { test } from "node:test";

import worker from "./mail.js";
import { htmlText, parse, words } from "./parse.js";

class FakeKV {
  constructor() {
    this.data = new Map();
    this.ttl = new Map();
  }
  async get(k) {
    return this.data.has(k) ? this.data.get(k) : null;
  }
  async put(k, v, opts = {}) {
    this.data.set(k, v);
    if (opts.expirationTtl) this.ttl.set(k, opts.expirationTtl);
  }
}

function message(raw, to = "Atlas@james.pointman.network") {
  const bytes = new TextEncoder().encode(raw);
  return {
    from: "bob@example.com",
    to,
    rawSize: bytes.length,
    raw: new Response(bytes).body,
    headers: new Headers({ "authentication-results": "mx.cloudflare.net; spf=pass; dkim=pass" }),
    rejected: null,
    setReject(why) {
      this.rejected = why;
    },
  };
}

const PLAIN = [
  "From: Bob <bob@example.com>",
  "To: atlas@james.pointman.network",
  "Subject: =?utf-8?B?SGVsbG8g8J+Riw==?=",
  "Authentication-Results: mx.cloudflare.net; spf=pass smtp.mailfrom=example.com; dkim=pass header.d=example.com",
  "Content-Type: text/plain; charset=utf-8",
  "Content-Transfer-Encoding: quoted-printable",
  "",
  "Hi Atlas,=0D=0Ayour code is 123456. Caf=C3=A9 soon=",
  "!",
].join("\r\n");

const MIXED = [
  "Subject: Report",
  'Content-Type: multipart/mixed; boundary="b1"',
  "",
  "--b1",
  'Content-Type: multipart/alternative; boundary="b2"',
  "",
  "--b2",
  "Content-Type: text/html; charset=utf-8",
  "",
  "<html><head><style>p{}</style></head><body><p>Only &amp; HTML</p><a href=\"https://x.test\">link</a></body></html>",
  "--b2--",
  "--b1",
  'Content-Type: application/pdf; name="report.pdf"',
  "Content-Disposition: attachment; filename=\"report.pdf\"",
  "Content-Transfer-Encoding: base64",
  "",
  "JVBERi0xLjQK",
  "--b1--",
  "",
].join("\r\n");

test("a plain mail keeps its words, subject and auth", () => {
  const m = parse(PLAIN);
  assert.equal(m.subject, "Hello 👋");
  assert.equal(m.text, "Hi Atlas,\nyour code is 123456. Café soon!");
  assert.match(m.auth, /spf=pass/);
  assert.deepEqual(m.attachments, []);
});

test("HTML only becomes text, and attachments are dropped but named", () => {
  const m = parse(MIXED);
  assert.equal(m.text, "Only & HTML\nlink (https://x.test)");
  assert.deepEqual(m.attachments, ["report.pdf"]);
  assert.ok(!m.text.includes("JVBER"));
});

test("encoded words and entities", () => {
  assert.equal(words("=?iso-8859-1?Q?Caf=E9_ok?="), "Café ok");
  assert.equal(htmlText("a&nbsp;&#x41;&#66;<br>c"), "a AB\nc");
});

test("the Worker stores the mail under its address, with the day's count and the latest key", async () => {
  const env = { MAIL: new FakeKV(), DAILY: "50", MAX_CHARS: "20000" };
  const m = message(PLAIN);
  await worker.email(m, env);
  assert.equal(m.rejected, null);
  const key = await env.MAIL.get("mail/latest");
  assert.match(key, /^mail\/atlas\/\d{13}-[0-9a-f]{12}$/);
  const stored = JSON.parse(await env.MAIL.get(key));
  assert.equal(stored.from, "bob@example.com");
  assert.equal(stored.to, "atlas@james.pointman.network");
  assert.equal(stored.subject, "Hello 👋");
  assert.equal(stored.cut, 0);
  const day = new Date().toISOString().slice(0, 10);
  assert.equal(await env.MAIL.get(`count/atlas/${day}`), "1");
  assert.equal(env.MAIL.ttl.get(`count/atlas/${day}`), 172800);
});

test("a long mail is cut, and says how much", async () => {
  const env = { MAIL: new FakeKV(), MAX_CHARS: "10" };
  await worker.email(message("Subject: x\r\n\r\n" + "é".repeat(25)), env);
  const stored = JSON.parse(await env.MAIL.get(await env.MAIL.get("mail/latest")));
  assert.equal(stored.text, "é".repeat(10));
  assert.equal(stored.cut, 15);
});

test("past the day's limit, mail is refused and nothing is written", async () => {
  const env = { MAIL: new FakeKV(), DAILY: "2" };
  for (let i = 0; i < 2; i++) await worker.email(message(PLAIN), env);
  const before = env.MAIL.data.size;
  const m = message(PLAIN);
  await worker.email(m, env);
  assert.match(m.rejected, /at most 2 mails a day/);
  assert.equal(env.MAIL.data.size, before);
});
