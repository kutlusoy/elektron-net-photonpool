//! Miner devices by the user agent they send with `mining.subscribe`: a display name, a link to the project and whether the
//! software and hardware are open source (the star in the device table). Unknown agents are listed under their own name.

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
    ("cgminer", "cgminer", "https://cgminer.info/", false),
    ("braiins", "Braiins OS", "https://braiins.com/os/plus", false),
    ("termux-miner", "termux-miner", "https://github.com/wong-fi-hung/termux-miner", false),
];

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

pub fn classify(agent: &str) -> Device {
    let agent = clean_agent(agent);
    let low = agent.to_ascii_lowercase();
    for (word, name, url, open) in KNOWN {
        if low.contains(word) {
            return Device { key: (*word).to_string(), name: (*name).to_string(), url: Some(url), open_source: *open };
        }
    }
    let token: String = agent.split(|c: char| c == '/' || c == ' ').next().unwrap_or("").to_string();
    let name = if token.is_empty() { "unknown".to_string() } else { token };
    Device { key: format!("other:{}", name.to_ascii_lowercase()), name, url: None, open_source: false }
}

#[cfg(test)]
mod tests {
    use super::*;

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
