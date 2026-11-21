//! Mail on Pointman, receiving only. Cloudflare's Email Routing hands each mail for a claimed address to the Email
//! Worker in worker/ (`WORKER_JS`, `PARSE_JS`), which stores it in Workers KV; the extension (src/main.rs) collects
//! it from there and posts it to its agent as outside mail (`words`). `cloudflare` is Cloudflare's API, and the
//! commands' params are proto/mail/v1/mail.proto's.

pub mod cloudflare;
pub mod words;

/// proto/mail/v1/mail.proto's messages, as buffa builds them (build.rs).
pub mod proto {
    pointman_extension::include_proto!();
}
pub use proto::mail::v1;

/// The Worker's entry module and the MIME reader it imports, uploaded as they are by `mail.setup`.
pub const WORKER_JS: &str = include_str!("../worker/mail.js");
pub const PARSE_JS: &str = include_str!("../worker/parse.js");

/// The vault item with the Cloudflare API token, account id and zone id.
pub const ITEM: &str = "mail.cloudflare";
/// The Worker's name and its KV namespace's title, unless the `worker` setting says another.
pub const WORKER: &str = "pointman-mail";
/// Mails a day an address takes, and a mail's characters kept, unless the settings say.
pub const DAILY: i32 = 50;
pub const MAX_CHARS: i32 = 20_000;
/// The key the Worker writes with the newest mail's key: the one read every poll.
pub const LATEST: &str = "mail/latest";
/// Workers KV's free tier: writes a day for the account, and the Worker's writes per mail (the mail, its count,
/// mail/latest). So the workspace's mails a day, at most, on the free tier.
pub const FREE_WRITES: i32 = 1_000;
pub const WRITES_PER_MAIL: i32 = 3;
pub const FREE_MAILS_A_DAY: i32 = FREE_WRITES / WRITES_PER_MAIL;
/// A routing rule's name at Cloudflare, at most (its API's limit), and a literal matcher's value.
pub const RULE_NAME_MAX: usize = 256;
pub const ADDRESS_MAX: usize = 90;

/// The vault item's value: the token, the account id, the zone id. Never logged.
#[derive(Clone)]
pub struct Creds {
    pub token: String,
    pub account: String,
    pub zone: String,
}

impl std::fmt::Debug for Creds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Creds").field("account", &self.account).field("zone", &self.zone).finish_non_exhaustive()
    }
}

impl Creds {
    pub fn parse(value: &str) -> Option<Creds> {
        let parts: Vec<&str> = value.split_whitespace().collect();
        let hex = |s: &str| s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit());
        let tokenish =
            |s: &str| (32..=200).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-".contains(c));
        match parts[..] {
            [t, a, z] if tokenish(t) && hex(a) && hex(z) => {
                Some(Creds { token: t.into(), account: a.to_ascii_lowercase(), zone: z.to_ascii_lowercase() })
            }
            _ => None,
        }
    }
}

/// A claimed address: Cloudflare's routing rule for it, which names its agent and its thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub rule: String,
    pub agent: String,
    pub address: String,
    pub thread: i64,
}

/// The rule's name, which keeps who the address is for and where its mail is posted: `pointman-mail atlas thr_123`.
pub fn rule_name(agent: &str, thread: i64) -> String {
    format!("pointman-mail {agent} thr_{thread}")
}

/// `rule_name`'s agent and thread.
pub fn parse_rule_name(name: &str) -> Option<(String, i64)> {
    let mut parts = name.split_whitespace();
    if parts.next()? != "pointman-mail" {
        return None;
    }
    let agent = parts.next()?.to_string();
    let thread = parts.next()?.strip_prefix("thr_")?.parse().ok().filter(|t| *t > 0)?;
    parts.next().is_none().then_some((agent, thread))
}

/// A thread as the claim gives it, `thr_123` or `123`.
pub fn thread_number(thread: &str) -> Option<i64> {
    let t = thread.trim();
    t.strip_prefix("thr_").unwrap_or(t).parse().ok().filter(|n| *n > 0)
}

/// A handle as the board reads a mention: lowercase letters, digits, `_` and `-`, at most 32.
pub fn handle_ok(h: &str) -> bool {
    let mut chars = h.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && h.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The local part of a stored mail's key, `mail/<local>/<time>-<id>`.
pub fn key_local(key: &str) -> Option<&str> {
    let mut parts = key.split('/');
    (parts.next()? == "mail").then_some(())?;
    let local = parts.next()?;
    parts.next()?;
    Some(local)
}

/// A list in words: `a`, `a and b`, `a, b and c`.
pub fn and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creds_are_a_token_an_account_and_a_zone() {
        let c = Creds::parse(&format!("{} {} {}", "t".repeat(40), "A".repeat(32), "b".repeat(32))).unwrap();
        assert_eq!((c.account.as_str(), c.zone.as_str()), ("a".repeat(32).as_str(), "b".repeat(32).as_str()));
        assert!(!format!("{c:?}").contains("tttt"), "the token is never in a debug line");
        assert!(Creds::parse("short a b").is_none());
        assert!(Creds::parse(&format!("{} {}", "t".repeat(40), "a".repeat(32))).is_none());
    }

    #[test]
    fn a_rule_name_keeps_its_agent_and_thread() {
        let name = rule_name("atlas", 71335);
        assert_eq!(name, "pointman-mail atlas thr_71335");
        assert_eq!(parse_rule_name(&name), Some(("atlas".into(), 71335)));
        assert!(rule_name(&"a".repeat(32), i64::MAX).len() <= RULE_NAME_MAX);
        assert_eq!(parse_rule_name("Send to bob rule"), None);
        assert_eq!(parse_rule_name("pointman-mail atlas thr_0"), None);
        assert_eq!(thread_number("thr_12"), Some(12));
        assert_eq!(thread_number("12"), Some(12));
        assert_eq!(thread_number("x"), None);
    }

    #[test]
    fn keys_and_handles() {
        assert_eq!(key_local("mail/atlas/0001-ab"), Some("atlas"));
        assert_eq!(key_local(LATEST), None);
        assert_eq!(key_local("count/atlas/2026-11-21"), None);
        assert!(handle_ok("vel29-term2") && !handle_ok("Atlas") && !handle_ok("a b"));
        assert_eq!(FREE_MAILS_A_DAY, 333);
    }
}
