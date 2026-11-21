//! extension.toml within what core's manifest check takes, and its schemas as proto/mail/v1/mail.proto says them
//! (the SDK's `proto::check`).

use regex::Regex;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn manifest() -> toml::Table {
    toml::from_str(&std::fs::read_to_string(format!("{ROOT}/extension.toml")).unwrap()).unwrap()
}

#[test]
fn the_manifest_keeps_to_cores_limits() {
    let m = manifest();
    let chars = |v: &toml::Value| v.as_str().unwrap().chars().count();
    let ext = &m["extension"];
    assert_eq!(ext["version"].as_str(), Some(env!("CARGO_PKG_VERSION")), "extension.toml's version is Cargo's");
    assert!((1..=60).contains(&chars(&ext["name"])) && (1..=200).contains(&chars(&ext["summary"])));
    assert_eq!(m["run"]["command"].as_array().unwrap()[0].as_str(), Some("bin/mail"), "a binary runs as bin/<id>");
    assert!(std::path::Path::new(ROOT).join("SKILL.md").is_file(), "SKILL.md ships with the behaviour");
    let slash = Regex::new("^[a-z][a-z0-9-]{0,23}$").unwrap();
    for c in m["commands"].as_array().unwrap() {
        let kind = c["kind"].as_str().unwrap();
        assert!(kind.starts_with("mail."), "{kind}");
        assert!(chars(&c["title"]) <= 60, "{kind}'s title is over 60");
        assert!(chars(&c["summary"]) <= 120, "{kind}'s summary is over 120");
        assert!(slash.is_match(c["slash"].as_str().unwrap()), "{kind}'s slash");
    }
    let creds = m["credentials"].as_array().unwrap();
    let forms: Vec<&str> = creds.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(forms, ["cloudflare"]);
    assert_eq!(creds[0]["item"].as_str(), Some("mail.cloudflare"));
    let takes = |example: &str| {
        Regex::new(&format!("^(?:{})$", creds[0]["value"].as_str().unwrap())).unwrap().is_match(example)
    };
    let (t, a, z) = ("T".repeat(40), "0123456789abcdef".repeat(2), "fedcba9876543210".repeat(2));
    assert!(takes(&format!("{t} {a} {z}")), "a token, the account id and the zone id");
    assert!(!takes(&format!("{t} {a}")), "all three");
    let help = creds[0]["help"].as_str().unwrap();
    for scope in ["Email Routing Rules", "Workers Scripts", "Workers KV Storage"] {
        assert!(help.contains(scope), "the form's help names the token's {scope} permission");
    }
    let perms = &m["permissions"];
    assert_eq!(perms["agents"].as_bool(), Some(true), "it reads the machine's agents");
    assert_eq!(perms["post"].as_bool(), Some(true), "it posts each mail");
}

/// The schemas are written from the proto by `pointman-extension manifest write`: this fails while they say something
/// else, and shows the file as it should be.
#[test]
fn the_schemas_are_the_protos() {
    if let Err(e) = pointman_extension::proto::check(ROOT.as_ref()) {
        let plan = pointman_extension::proto::plan(ROOT.as_ref()).map(|p| p.text).unwrap_or_default();
        panic!("{e:#}\n\nextension.toml as the proto writes it:\n{plan}");
    }
}
