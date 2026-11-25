//! Cloudflare's API, as far as mail needs it: Email Routing rules on the zone, the Worker script and its KV
//! namespace on the account, KV's keys and values, and the domain's MX records over DNS over HTTPS. Errors say what
//! Cloudflare said, never the token.

use std::io::Read;
use std::time::Duration;

use anyhow::{anyhow, bail};
use serde_json::{json, Value};

use crate::{parse_rule_name, Claim, Creds};

/// Where the calls go: Cloudflare's own, unless a test's stand-in is named in the environment (the node never
/// passes its own environment on, so only a test can).
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// `{api}/zones/…`, `{api}/accounts/…`.
    pub api: String,
    /// DNS over HTTPS, JSON.
    pub doh: String,
}

impl Endpoints {
    pub fn from_env() -> Self {
        let get = |k: &str, default: &str| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| default.into())
                .trim_end_matches('/')
                .to_string()
        };
        Endpoints {
            api: get("CLOUDFLARE_API_BASE", "https://api.cloudflare.com/client/v4"),
            doh: get("MAIL_DOH_URL", "https://cloudflare-dns.com/dns-query"),
        }
    }
}

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .user_agent(concat!("pointman-mail/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// A path segment, percent-encoded: KV keys have slashes.
pub fn segment(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn text(r: ureq::Response) -> String {
    let mut s = String::new();
    let _ = r.into_reader().take(32 << 20).read_to_string(&mut s);
    s
}

/// What Cloudflare's errors say: `message (code)`, each, at most 300 characters.
fn said(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let each: Vec<String> = v["errors"]
        .as_array()
        .map(|es| {
            es.iter()
                .map(|e| match e["code"].as_i64() {
                    Some(c) => format!("{} ({c})", e["message"].as_str().unwrap_or("")),
                    None => e["message"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let words = if each.is_empty() { body.trim().to_string() } else { each.join("; ") };
    words.chars().take(300).collect()
}

/// A rule's matcher address and action, as Cloudflare lists it.
fn rule_parts(r: &Value) -> (Option<String>, Option<(String, String)>) {
    let to = r["matchers"].as_array().and_then(|ms| {
        ms.iter()
            .find(|m| m["type"] == "literal" && m["field"] == "to")
            .and_then(|m| m["value"].as_str())
            .map(|v| v.to_ascii_lowercase())
    });
    let action = r["actions"].as_array().and_then(|a| a.first()).map(|a| {
        let value = a["value"].as_array().and_then(|v| v.first()).and_then(Value::as_str).unwrap_or_default();
        (a["type"].as_str().unwrap_or_default().to_string(), value.to_string())
    });
    (to, action)
}

/// A routing rule with one literal address, as Cloudflare lists it.
#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub to: String,
    /// Its action's type (`worker`, `forward`, `drop`) and value (the Worker's name).
    pub kind: String,
    pub value: String,
    pub name: String,
}

/// One account and zone's API, with its token.
pub struct Cloudflare<'a> {
    pub agent: &'a ureq::Agent,
    pub ends: &'a Endpoints,
    pub creds: &'a Creds,
}

impl Cloudflare<'_> {
    fn request(&self, method: &str, path: &str) -> ureq::Request {
        self.agent
            .request(method, &format!("{}{path}", self.ends.api))
            .set("Authorization", &format!("Bearer {}", self.creds.token))
    }

    fn answer(&self, what: &str, r: Result<ureq::Response, ureq::Error>) -> anyhow::Result<Value> {
        let body = match r {
            Ok(r) => text(r),
            Err(ureq::Error::Status(code, r)) => {
                let body = text(r);
                let hint = match code {
                    401 | 403 => ": check the token's permissions (/mail add shows them)",
                    _ => "",
                };
                bail!("Cloudflare refused {what} ({code}): {}{hint}", said(&body));
            }
            Err(e) => bail!("couldn't reach Cloudflare for {what}: {}", e.kind()),
        };
        let v: Value = serde_json::from_str(&body).map_err(|_| anyhow!("Cloudflare's answer to {what} isn't JSON"))?;
        if v["success"] == false {
            bail!("Cloudflare refused {what}: {}", said(&body));
        }
        Ok(v)
    }

    fn call(&self, what: &str, method: &str, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let req = self.request(method, path);
        let r = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };
        self.answer(what, r)
    }

    /// Every page of a list, `page` and `per_page` as Cloudflare pages them.
    fn pages(&self, what: &str, path: &str, per_page: u32) -> anyhow::Result<Vec<Value>> {
        let mut out = Vec::new();
        for page in 1..=50 {
            let sep = if path.contains('?') { '&' } else { '?' };
            let v = self.call(what, "GET", &format!("{path}{sep}page={page}&per_page={per_page}"), None)?;
            let items = v["result"].as_array().cloned().unwrap_or_default();
            let n = items.len();
            out.extend(items);
            let total = v["result_info"]["total_pages"].as_u64().unwrap_or(1);
            if n == 0 || page as u64 >= total {
                break;
            }
        }
        Ok(out)
    }

    fn account(&self) -> String {
        format!("/accounts/{}", self.creds.account)
    }

    // ---- the Worker and its KV namespace

    /// The KV namespace with this title: its id.
    pub fn namespace(&self, title: &str) -> anyhow::Result<Option<String>> {
        let all = self.pages("the KV namespaces", &format!("{}/storage/kv/namespaces", self.account()), 100)?;
        Ok(all.iter().find(|n| n["title"] == title).and_then(|n| n["id"].as_str()).map(String::from))
    }

    pub fn make_namespace(&self, title: &str) -> anyhow::Result<String> {
        let v = self.call(
            "making the KV namespace",
            "POST",
            &format!("{}/storage/kv/namespaces", self.account()),
            Some(json!({"title": title})),
        )?;
        v["result"]["id"].as_str().map(String::from).ok_or_else(|| anyhow!("Cloudflare made no KV namespace id"))
    }

    pub fn worker_exists(&self, name: &str) -> anyhow::Result<bool> {
        let v = self.call("the Worker scripts", "GET", &format!("{}/workers/scripts", self.account()), None)?;
        Ok(v["result"].as_array().is_some_and(|s| s.iter().any(|w| w["id"] == name)))
    }

    /// The Worker's modules, uploaded with its bindings: the KV namespace as MAIL, and the limits.
    pub fn put_worker(&self, name: &str, metadata: &Value, modules: &[(&str, &str)]) -> anyhow::Result<()> {
        let boundary = "pointman-mail-6f1c2b9e0d4a";
        let mut body = String::new();
        let mut part = |name: &str, filename: Option<&str>, kind: &str, content: &str| {
            body.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\""));
            if let Some(f) = filename {
                body.push_str(&format!("; filename=\"{f}\""));
            }
            body.push_str(&format!("\r\nContent-Type: {kind}\r\n\r\n{content}\r\n"));
        };
        part("metadata", None, "application/json", &metadata.to_string());
        for (file, content) in modules {
            part(file, Some(file), "application/javascript+module", content);
        }
        body.push_str(&format!("--{boundary}--\r\n"));
        let r = self
            .request("PUT", &format!("{}/workers/scripts/{}", self.account(), segment(name)))
            .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
            .send_string(&body);
        self.answer("uploading the Worker", r).map(|_| ())
    }

    // ---- KV

    fn kv(&self, ns: &str) -> String {
        format!("{}/storage/kv/namespaces/{ns}", self.account())
    }

    /// A value, or None when there's no such key.
    pub fn get(&self, ns: &str, key: &str) -> anyhow::Result<Option<String>> {
        let r = self.request("GET", &format!("{}/values/{}", self.kv(ns), segment(key))).call();
        match r {
            Ok(r) => Ok(Some(text(r))),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(ureq::Error::Status(code, r)) => {
                bail!("Cloudflare refused reading {key} from KV ({code}): {}", said(&text(r)))
            }
            Err(e) => bail!("couldn't reach Cloudflare for KV: {}", e.kind()),
        }
    }

    /// Every key under a prefix, sorted (one list request per 1,000).
    pub fn keys(&self, ns: &str, prefix: &str) -> anyhow::Result<Vec<String>> {
        let mut out = Vec::new();
        let mut cursor = String::new();
        for _ in 0..20 {
            let mut path = format!("{}/keys?limit=1000&prefix={}", self.kv(ns), segment(prefix));
            if !cursor.is_empty() {
                path.push_str(&format!("&cursor={}", segment(&cursor)));
            }
            let v = self.call("listing KV's keys", "GET", &path, None)?;
            out.extend(
                v["result"].as_array().into_iter().flatten().filter_map(|k| k["name"].as_str()).map(String::from),
            );
            cursor = v["result_info"]["cursor"].as_str().unwrap_or_default().to_string();
            if cursor.is_empty() {
                break;
            }
        }
        out.sort();
        Ok(out)
    }

    pub fn delete(&self, ns: &str, key: &str) -> anyhow::Result<()> {
        let r = self.request("DELETE", &format!("{}/values/{}", self.kv(ns), segment(key))).call();
        match r {
            Err(ureq::Error::Status(404, _)) => Ok(()),
            r => self.answer("deleting a mail from KV", r).map(|_| ()),
        }
    }

    // ---- Email Routing rules

    fn rules_path(&self) -> String {
        format!("/zones/{}/email/routing/rules", self.creds.zone)
    }

    /// Every routing rule with a literal address.
    pub fn rules(&self) -> anyhow::Result<Vec<Rule>> {
        let all = self.pages("the Email Routing rules", &self.rules_path(), 50)?;
        Ok(all
            .iter()
            .filter_map(|r| {
                let (to, action) = rule_parts(r);
                let (kind, value) = action.unwrap_or_default();
                Some(Rule {
                    id: r["id"].as_str().or(r["tag"].as_str())?.to_string(),
                    to: to?,
                    kind,
                    value,
                    name: r["name"].as_str().unwrap_or_default().to_string(),
                })
            })
            .collect())
    }

    /// The addresses claimed through this extension: rules that send one address on `domain` to `worker`, and whose
    /// name says its agent and channel.
    pub fn claims(&self, worker: &str, domain: &str) -> anyhow::Result<Vec<Claim>> {
        let at = format!("@{domain}");
        Ok(self
            .rules()?
            .into_iter()
            .filter(|r| r.kind == "worker" && r.value == worker && r.to.ends_with(&at))
            .filter_map(|r| {
                let (agent, channel) = parse_rule_name(&r.name)?;
                (r.to == format!("{agent}{at}")).then_some(Claim { rule: r.id, agent, address: r.to, channel })
            })
            .collect())
    }

    pub fn add_rule(&self, name: &str, address: &str, worker: &str) -> anyhow::Result<String> {
        let body = json!({
            "name": name,
            "enabled": true,
            "matchers": [{"type": "literal", "field": "to", "value": address}],
            "actions": [{"type": "worker", "value": [worker]}],
        });
        let v = self.call("adding the routing rule", "POST", &self.rules_path(), Some(body))?;
        Ok(v["result"]["id"].as_str().or(v["result"]["tag"].as_str()).unwrap_or_default().to_string())
    }

    pub fn delete_rule(&self, id: &str) -> anyhow::Result<()> {
        self.call("removing the routing rule", "DELETE", &format!("{}/{}", self.rules_path(), segment(id)), None)
            .map(|_| ())
    }
}

/// The domain's MX records, over DNS over HTTPS (no token needed): each `priority host`.
pub fn mx(agent: &ureq::Agent, ends: &Endpoints, domain: &str) -> anyhow::Result<Vec<String>> {
    let r = agent
        .get(&format!("{}?name={}&type=MX", ends.doh, segment(domain)))
        .set("Accept", "application/dns-json")
        .call()
        .map_err(|e| anyhow!("couldn't look up {domain}'s mail records: {e}"))?;
    let v: Value = serde_json::from_str(&text(r)).map_err(|_| anyhow!("the DNS answer for {domain} isn't JSON"))?;
    Ok(v["Answer"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["type"] == 15)
        .filter_map(|a| a["data"].as_str())
        .map(|d| d.trim_end_matches('.').to_ascii_lowercase())
        .collect())
}

/// Whether MX records send the domain's mail to Cloudflare's Email Routing.
pub fn takes_mail(records: &[String]) -> bool {
    records.iter().any(|r| r.ends_with("mx.cloudflare.net"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_one_segment() {
        assert_eq!(segment("mail/atlas/0001-ab"), "mail%2Fatlas%2F0001-ab");
        assert_eq!(segment("a b@c"), "a%20b%40c");
    }

    #[test]
    fn cloudflares_errors_in_words() {
        let body = r#"{"success":false,"errors":[{"code":10000,"message":"Authentication error"}]}"#;
        assert_eq!(said(body), "Authentication error (10000)");
        assert_eq!(said("plain"), "plain");
    }

    #[test]
    fn mail_records() {
        assert!(takes_mail(&["13 route1.mx.cloudflare.net".into()]));
        assert!(!takes_mail(&["10 mx.google.com".into()]));
    }
}
