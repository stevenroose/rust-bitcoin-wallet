
use bitcoin::Network;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
pub struct WalletConfig {
	pub network: Network,
}
