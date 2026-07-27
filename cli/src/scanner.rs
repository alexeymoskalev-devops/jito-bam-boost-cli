use serde::{Deserialize, Serialize};

/// Status of one epoch's subsidy for a claimant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochStatus {
    pub epoch: u64,
    /// Allocation in JitoSOL lamports; `None` = claimant not in that epoch's tree.
    pub amount: Option<u64>,
    pub claimed: bool,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct ListResponse {
    #[serde(default)]
    items: Vec<ListItem>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct ListItem {
    name: String,
}

/// Extracts the epoch from a GCS object name like `mainnet/1000/merkle_tree.json`.
pub fn parse_epoch_from_object_name(name: &str, network: &str) -> Option<u64> {
    let rest = name.strip_prefix(network)?.strip_prefix('/')?;
    let (epoch_str, file) = rest.split_once('/')?;
    if file != "merkle_tree.json" {
        return None;
    }
    epoch_str.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_epoch_from_object_name() {
        assert_eq!(
            parse_epoch_from_object_name("mainnet/1000/merkle_tree.json", "mainnet"),
            Some(1000)
        );
    }

    #[test]
    fn ignores_directory_placeholder_and_foreign_names() {
        assert_eq!(parse_epoch_from_object_name("mainnet/", "mainnet"), None);
        assert_eq!(
            parse_epoch_from_object_name("testnet/900/merkle_tree.json", "mainnet"),
            None
        );
        assert_eq!(
            parse_epoch_from_object_name("mainnet/abc/merkle_tree.json", "mainnet"),
            None
        );
    }
}
