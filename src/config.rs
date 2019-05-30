
use bitcoin::Network;
use serde::{Serialize, Deserialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct WalletConfig {
	pub network: Network,
}
