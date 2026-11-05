//! Miner devices by the user agent they send with `mining.subscribe`: a display name, a link to the project and whether the
//! software and hardware are open source (the star in the device table). Unknown agents are listed under their own name, but only for the first `MAX_OTHER` different names; every further one is counted as "Other", so random agents cannot blow up the statistics.

/// (word searched in the lower-cased agent, name, project link, open source software and hardware). More specific words come first.
const KNOWN: &[(&str, &str, &str, bool)] = &[
    ("nerdminer", "NerdMiner", "https://github.com/BitMaker-hub/NerdMiner_v2", true),
    ("nerdnos", "NerdNOS", "https://github.com/WantClue/NerdMiner_v2/tree/nerdnos", true),
    ("nerdoctaxe", "NerdOCTAXE", "https://github.com/Patsch91/NerdOCTAXE-Gamma", true),
    ("nerdeko", "NerdEKO", "https://github.com/phil31/NerdEKO-Gamma", true),
    ("nerdqaxe++", "NerdQAxe++", "https://github.com/shufps/qaxe", true),
    ("nerdqaxe+", "NerdQAxe+", "https://github.com/shufps/qaxe", true),
    ("nerdaxegamma", "NerdAxeGamma", "https://github.com/BitMaker-hub/NerdAxe", true),
    ("nerdaxe", "NerdAxe", "https://github.com/BitMaker-hub/NerdAxeUltra", true),
    ("piaxe", "PiAxe", "https://github.com/shufps/piaxe", true),
    ("qaxe+", "QAxe+", "https://github.com/shufps/qaxe", true),
    ("qaxe", "QAxe", "https://github.com/shufps/qaxe", true),
    ("0xaxe", "0xAxe", "https://github.com/shufps/0xaxe", true),
    ("bitaxehex", "Bitaxe Hex", "https://github.com/skot/bitaxeHex/", true),
    ("bitaxe", "Bitaxe", "https://github.com/skot/bitaxe/", true),
    ("leafminer", "LeafMiner", "https://github.com/matteocrippa/leafminer", true),
    ("cpuminer", "cpuminer", "https://github.com/pooler/cpuminer", false),
    ("antminer", "Antminer", "https://www.bitmain.com/", false),
    ("bmminer", "Bitmain bmminer", "https://www.bitmain.com/", false),
    ("whatsminer", "Whatsminer", "https://www.microbt.com/", false),
    ("avalon", "Avalon", "https://www.canaan.io/", false),
    ("cgminer", "cgminer", "https://cgminer.info/", false),
    ("braiins", "Braiins OS", "https://braiins.com/os/plus", false),
    ("termux-miner", "termux-miner", "https://github.com/wong-fi-hung/termux-miner", false),
];

/// Distinct names of unknown agents that get their own row (the rest is "Other").
const MAX_OTHER: usize = 24;
static OTHER: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::LazyLock::new(Default::default);

#[derive(Debug, PartialEq, Clone)]
pub struct Device {
    /// Stable key for grouping and storage.
    pub key: String,
    pub name: String,
    pub url: Option<&'static str>,
    pub open_source: bool,
}

/// Printable ASCII only and at most 64 characters: the agent is shown on public pages.
pub fn clean_agent(agent: &str) -> String {
    agent.chars().filter(|c| c.is_ascii_graphic() || *c == ' ').take(64).collect::<String>().trim().to_string()
}

/// Key under which the pool remembers what it has learned about a kind of device: the chip for ESP-Miner devices (`bitaxe/bm1370`), else the software name.
pub fn profile_key(agent: &str) -> String {
    let a = clean_agent(agent).to_ascii_lowercase();
    let mut parts = a.split(|c: char| c == '/' || c == ' ').filter(|p| !p.is_empty());
    let first = parts.next().unwrap_or("unknown");
    if first == "bitaxe" || first == "esp-miner" {
        if let Some(chip) = parts.next().filter(|p| p.starts_with("bm")) {
            return format!("bitaxe/{chip}");
        }
    }
    first.to_string()
}

/// Typical hash rate of a device that tells its chip in the user agent (ESP-Miner family: `bitaxe/BM1370/v2.x`). Only a starting value so that the first job
/// interval and the first share difficulty fit before the pool has measured anything; the pool learns the real rate from the shares and corrects it.
pub fn expected_hashrate(agent: &str) -> Option<f64> {
    let a = agent.to_ascii_lowercase();
    if !(a.contains("bitaxe") || a.contains("esp-miner")) {
        return None;
    }
    [("bm1370", 1.2e12), ("bm1368", 0.7e12), ("bm1366", 0.5e12), ("bm1397", 0.4e12)].iter().find(|(chip, _)| a.contains(chip)).map(|(_, h)| *h)
}

pub fn classify(agent: &str) -> Device {
    classify_in(agent, &OTHER, MAX_OTHER)
}

fn classify_in(agent: &str, other: &std::sync::Mutex<std::collections::HashSet<String>>, max_other: usize) -> Device {
    let agent = clean_agent(agent);
    let low = agent.to_ascii_lowercase();
    for (word, name, url, open) in KNOWN {
        if low.contains(word) {
            return Device { key: (*word).to_string(), name: (*name).to_string(), url: Some(url), open_source: *open };
        }
    }
    let token: String = agent.split(|c: char| c == '/' || c == ' ').next().unwrap_or("").to_string();
    let name = if token.is_empty() { "unknown".to_string() } else { token };
    let key = format!("other:{}", name.to_ascii_lowercase());
    let mut seen = other.lock().unwrap();
    if seen.contains(&key) || (seen.len() < max_other && seen.insert(key.clone())) {
        return Device { key, name, url: None, open_source: false };
    }
    Device { key: "other".to_string(), name: "Other".to_string(), url: None, open_source: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_agents_are_capped() {
        let set = std::sync::Mutex::new(std::collections::HashSet::new());
        for i in 0..3 {
            assert_eq!(classify_in(&format!("weird{i}/1.0"), &set, 3).key, format!("other:weird{i}"));
        }
        assert_eq!(classify_in("brandnew/1.0", &set, 3).key, "other");
        assert_eq!(classify_in("brandnew/1.0", &set, 3).name, "Other");
        assert_eq!(classify_in("weird1/2.0", &set, 3).key, "other:weird1");
        assert_eq!(classify("Antminer S19").name, "Antminer");
    }

    #[test]
    fn profile_key_names_the_chip_or_the_software() {
        assert_eq!(profile_key("bitaxe/BM1370/v2.9.0"), "bitaxe/bm1370");
        assert_eq!(profile_key("ESP-Miner/BM1366 v2"), "bitaxe/bm1366");
        assert_eq!(profile_key("cgminer/4.11.1"), "cgminer");
        assert_eq!(profile_key(""), "unknown");
    }

    #[test]
    fn chip_in_the_agent_gives_a_starting_hash_rate() {
        assert_eq!(expected_hashrate("bitaxe/BM1370/v2.9.0"), Some(1.2e12));
        assert_eq!(expected_hashrate("bitaxe/BM1366/v2.1"), Some(0.5e12));
        assert_eq!(expected_hashrate("cpuminer/2.5"), None);
        assert_eq!(expected_hashrate("bitaxe/unknownchip"), None);
    }

    #[test]
    fn known_agents_are_named_and_linked() {
        let d = classify("bitaxe/BM1370/v2.9.0");
        assert_eq!((d.name.as_str(), d.open_source), ("Bitaxe", true));
        assert_eq!(d.url, Some("https://github.com/skot/bitaxe/"));
        assert_eq!(classify("NerdMiner/1.0").name, "NerdMiner");
        assert_eq!(classify("bitaxeHex v1").name, "Bitaxe Hex");
        assert_eq!(classify("NerdQAxe++/2.0").name, "NerdQAxe++");
        assert_eq!(classify("NerdQAxe+/2.0").name, "NerdQAxe+");
        assert!(!classify("cpuminer-multi/1.3").open_source);
    }

    #[test]
    fn unknown_and_hostile_agents_are_shown_safely() {
        let d = classify("MyMiner/9.1 (x)");
        assert_eq!((d.name.as_str(), d.key.as_str(), d.url), ("MyMiner", "other:myminer", None));
        assert_eq!(classify("").name, "unknown");
        assert_eq!(clean_agent("<script>\u{0}\n bad\u{e9}"), "<script> bad");
        assert!(clean_agent(&"x".repeat(500)).len() <= 64);
    }
}
