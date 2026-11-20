// A mail's text from its raw MIME message, with nothing but the Workers runtime: its headers, its text/plain part
// (else its text/html part as text), and the names of its attachments, which are dropped. Kept small on purpose: it
// reads what mail clients send (multipart, base64, quoted-printable, RFC 2047 words, any charset TextDecoder knows),
// and anything it can't read stays as it came.

/** Bytes as a string of char codes 0..255, so nothing is lost before a part's own charset is known. */
export function latin1(bytes) {
  let out = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    out += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  }
  return out;
}

function bytesOf(binary) {
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i) & 0xff;
  return out;
}

function decode(binary, charset) {
  try {
    return new TextDecoder((charset || "utf-8").trim().toLowerCase(), { fatal: false }).decode(bytesOf(binary));
  } catch {
    return new TextDecoder("utf-8").decode(bytesOf(binary));
  }
}

function base64(text) {
  try {
    return atob(text.replace(/[^A-Za-z0-9+/=]/g, ""));
  } catch {
    return text;
  }
}

function quotedPrintable(text, header = false) {
  if (header) text = text.replace(/_/g, " ");
  return text.replace(/=\r?\n/g, "").replace(/=([0-9A-Fa-f]{2})/g, (_, h) => String.fromCharCode(parseInt(h, 16)));
}

/** A header's RFC 2047 encoded words (=?utf-8?B?…?=) as text. */
export function words(value) {
  return value
    .replace(/\?=\s+=\?/g, "?==?")
    .replace(/=\?([^?]+)\?([BbQq])\?([^?]*)\?=/g, (_, charset, enc, text) =>
      decode(enc.toUpperCase() === "B" ? base64(text) : quotedPrintable(text, true), charset.split("*")[0]),
    );
}

/** Header lines (unfolded) and the body, split at the first empty line. */
function split(binary) {
  const m = /\r?\n\r?\n/.exec(binary);
  const head = m ? binary.slice(0, m.index) : binary;
  const body = m ? binary.slice(m.index + m[0].length) : "";
  const headers = [];
  for (const line of head.split(/\r?\n/)) {
    if (/^[ \t]/.test(line) && headers.length) headers[headers.length - 1][1] += " " + line.trim();
    else {
      const i = line.indexOf(":");
      if (i > 0) headers.push([line.slice(0, i).trim().toLowerCase(), line.slice(i + 1).trim()]);
    }
  }
  return { headers, body };
}

function header(headers, name) {
  const h = headers.find(([k]) => k === name);
  return h ? h[1] : "";
}

/** `text/plain; charset="utf-8"` as its type and its parameters. */
function params(value) {
  const [type, ...rest] = value.split(";");
  const out = { type: type.trim().toLowerCase() };
  for (const p of rest) {
    const i = p.indexOf("=");
    if (i > 0) out[p.slice(0, i).trim().toLowerCase()] = p.slice(i + 1).trim().replace(/^"(.*)"$/, "$1");
  }
  return out;
}

/** HTML as plain text: scripts and styles go, blocks become lines, entities are read. */
export function htmlText(html) {
  const named = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " " };
  return html
    .replace(/<(script|style|head)[^>]*>[\s\S]*?<\/\1>/gi, "")
    .replace(/<br\s*\/?>/gi, "\n")
    .replace(/<\/(p|div|tr|li|h[1-6]|table|blockquote)>/gi, "\n")
    .replace(/<li[^>]*>/gi, "- ")
    .replace(/<a\s[^>]*href="([^"]*)"[^>]*>([\s\S]*?)<\/a>/gi, (_, href, text) => `${text} (${href})`)
    .replace(/<[^>]+>/g, "")
    .replace(/&(#x?[0-9a-f]+|[a-z]+);/gi, (all, e) => {
      if (e[0] === "#") {
        const n = e[1] === "x" || e[1] === "X" ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10);
        return Number.isFinite(n) ? String.fromCodePoint(n) : all;
      }
      return named[e.toLowerCase()] ?? all;
    })
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

/** Walks a part: text parts into `found`, attachments' names into `files`. */
function walk(binary, found, files, depth = 0) {
  const { headers, body } = split(binary);
  const type = params(header(headers, "content-type") || "text/plain");
  const disposition = params(header(headers, "content-disposition") || "inline");
  const name = disposition.filename || type.name;
  if (type.type.startsWith("multipart/") && type.boundary && depth < 10) {
    const parts = body.split(new RegExp(`\\r?\\n?--${type.boundary.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`));
    for (const part of parts.slice(1)) {
      if (part.startsWith("--")) break;
      walk(part.replace(/^[ \t]*\r?\n/, ""), found, files, depth + 1);
    }
    return;
  }
  if (disposition.type === "attachment" || (name && !type.type.startsWith("text/"))) {
    if (name) files.push(words(name));
    return;
  }
  if (type.type !== "text/plain" && type.type !== "text/html") return;
  const enc = header(headers, "content-transfer-encoding").toLowerCase();
  const raw = enc === "base64" ? base64(body) : enc === "quoted-printable" ? quotedPrintable(body) : body;
  const text = decode(raw, type.charset);
  (type.type === "text/plain" ? found.plain : found.html).push(text);
}

/** A raw message (a binary string) as {subject, date, messageId, auth, text, attachments}. */
export function parse(binary) {
  const { headers } = split(binary);
  const found = { plain: [], html: [] };
  const files = [];
  walk(binary, found, files);
  const text = found.plain.length ? found.plain.join("\n\n") : found.html.map(htmlText).join("\n\n");
  return {
    subject: words(header(headers, "subject")),
    date: header(headers, "date"),
    messageId: header(headers, "message-id"),
    auth: headers.filter(([k]) => k === "authentication-results").map(([, v]) => v).join("; "),
    text: text.replace(/\r\n/g, "\n").trim(),
    attachments: files,
  };
}
