use std::collections::HashMap;

/// Read-only configuration lookup.
///
/// Values come from the workflow node definition. Components can read
/// their configuration but never modify it.
pub trait ConfigHost: Send + Sync {
    fn get(&self, key: &str) -> Option<&str>;
}

/// Simple map-backed config implementation.
#[derive(Debug, Default)]
pub struct MapConfig {
    values: HashMap<String, String>,
}

impl MapConfig {
    pub fn new(values: HashMap<String, String>) -> Self {
        Self { values }
    }
}

impl ConfigHost for MapConfig {
    fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_config() {
        let mut values = HashMap::new();
        values.insert("api_key".to_string(), "secret".to_string());
        values.insert("base_url".to_string(), "https://example.com".to_string());

        let config = MapConfig::new(values);

        assert_eq!(config.get("api_key"), Some("secret"));
        assert_eq!(config.get("base_url"), Some("https://example.com"));
        assert_eq!(config.get("missing"), None);
    }
}
