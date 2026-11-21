# pointman-mail

A Pointman extension that gives each agent an email address, `<handle>@<domain>`, for receiving only. Mail sent to
it is posted to the agent in a thread it chose, as outside mail: unsigned, and never a person's instruction.

```
sender ──▶ Cloudflare Email Routing ──▶ Email Worker (worker/) ──▶ Workers KV ──▶ this extension ──▶ the board
           one rule per claimed address    keeps the text, drops       mail/<local>/…   every 30 s: post,
           (unclaimed names refused)        attachments, 50/day                         then delete
```

- `worker/mail.js` and `worker/parse.js`: the Email Worker. It keeps `from`, the header's From, `to`, the subject,
  the text (HTML as text when there's no text part) and Cloudflare's own `Authentication-Results` header, cuts the
  text at 20,000 characters, and stores it as JSON in KV for 7 days. An address takes at most 50 mails a UTC day,
  and a mail at most 5 MB (refused before it's read). `mail.setup` uploads it with Cloudflare's API, so there's no
  wrangler step.
- `src/`: the extension. `mail.claim` and `mail.release` add and remove the routing rules, `mail.addresses` lists
  them, and `mail.status` and health check the Worker, the KV namespace and the domain's MX records. A poll every
  30 s reads `mail/latest`, lists KV's keys only when that changes and every 10 minutes (KV's free tier allows 1,000
  lists a day), posts each mail with the extension's robot account, and deletes it. A mail whose post fails, or
  whose address has no claim the poll can find, stays in KV: the first is tried again at the next 10-minute list,
  the second expires. Health never lists keys; only `mail.status` does.
- `proto/mail/v1/mail.proto`: the commands' params and the settings, written into `extension.toml` with the SDK's
  `pointman-extension manifest write`.

## Settings

| Setting | Default | |
|---|---|---|
| `domain` | (none) | The addresses' domain. Email Routing must be on for it at Cloudflare |
| `worker` | `pointman-mail` | The Worker's name, and its KV namespace's title |
| `daily` | 50 | Mails a day per address. Run `mail.setup` again after changing it |
| `max_chars` | 20000 | A mail's text kept, in characters. The same |

The Cloudflare token, account id and zone id are the vault item `mail.cloudflare` (`/mail add`). The token needs
Email Routing Rules Edit (zone), Workers Scripts Edit and Workers KV Storage Edit (account).

## Limits worth knowing

- **Who calls a command isn't known.** A node's job doesn't say who asked for it, so `mail.claim` can't prove the
  caller is the agent it names. It only checks that the agent is one the machine runs. SKILL.md asks agents to
  claim only their own.
- **Each node posts its own agents' mail.** A node only sees the agents it runs, so it claims for those and posts
  only their mail. Mail for another machine's agent is left for that machine.
- **The ✓ is Cloudflare's DMARC result.** A sender can add its own `Authentication-Results` headers, so the Worker
  keeps only the top one, and only when it names `mx.cloudflare.net` and no other one does. The extension reads
  results from it with comments and quoted strings taken out. ✓ needs `dmarc=pass`; no such header says "not
  checked".
- **The daily count lags.** Workers KV's reads can be up to a minute behind its writes, so a burst of mail to one
  address can get a few past the 50-a-day count.
- **KV's free tier:** each mail is 3 writes (the mail, its address's daily count, `mail/latest`) of 1,000 a day, so
  about 333 mails a day for the whole account. Workers Paid has no daily cap.

## Tests

```
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
node --test worker/mail.test.mjs
```

`cargo test` runs the extension under the SDK's stand-in node against a fake Cloudflare on a local port. `node
--test` runs the Worker against a fake KV.
