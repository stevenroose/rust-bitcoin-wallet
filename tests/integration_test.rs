extern crate bitcoin;
extern crate bitcoin_hashes;
extern crate byteorder;
extern crate hex;
#[macro_use]
extern crate lazy_static;
extern crate rand;
extern crate secp256k1;
extern crate serde;
// dev-deps
extern crate serde_json;
extern crate bitcoincore_rpc;
// self
extern crate bitcoin_wallet;

use std::{env, io};

use bitcoin::util::bip32;
use bitcoin::{Block, Network, TxOut};
use bitcoin::consensus::encode::serialize;
use bitcoincore_rpc::RpcApi;

use bitcoin_wallet::config::WalletConfig;
use bitcoin_wallet::wallet::Wallet;

lazy_static! {
	static ref SECP: secp256k1::Secp256k1<secp256k1::All> = secp256k1::Secp256k1::new();
}

const XPUB_PATH: &str = "m/0'/0'";
const BASE_PATH: &str = "m/0";

fn init_bitcoind() -> bitcoincore_rpc::Client {
	let bitcoind_host = env::var("BITCOIND_HOST").expect("BITCOIND_HOST missing");
	let bitcoind_cookie = env::var("BITCOIND_COOKIE").expect("BITCOIND_COOKIE missing");
	let bitcoind_auth = bitcoincore_rpc::Auth::CookieFile(bitcoind_cookie.into());
	bitcoincore_rpc::Client::new(bitcoind_host, bitcoind_auth).expect("RPC error")
}

fn init_wallet() -> (bip32::ExtendedPrivKey, Wallet) {
	let config = WalletConfig {
		network: Network::Regtest,
	};

	let seed =
		hex::decode("d7e6ab0cb485ab6e73975626d2d8e7a92d8643b873feef202306ee1bd4121683").unwrap();
	let xpriv = bip32::ExtendedPrivKey::new_master(Network::Regtest, &seed).unwrap();
	let xpub = bip32::ExtendedPubKey::from_private(
		&SECP,
		&xpriv.derive_priv(&SECP, &XPUB_PATH.parse::<bip32::DerivationPath>().unwrap()).unwrap(),
	);
	let wallet = Wallet::new(config, xpub, xpriv.fingerprint(&SECP), BASE_PATH.parse().unwrap());

	(xpriv, wallet)
}

fn dump_wallet(wallet: &Wallet) {
	println!("--------------------");
	serde_json::to_writer_pretty(io::stdout(), wallet).expect("dump_wallet error");
	println!("--------------------");
}

fn generate(bitcoind: &bitcoincore_rpc::Client) -> Block {
	let generate_addr = bitcoind.get_new_address(None, None).expect("RPR");
	let block_hashes = bitcoind.generate_to_address(1, &generate_addr).expect("RPC");
	assert_eq!(block_hashes.len(), 1);
	bitcoind.get_block(&block_hashes[0]).expect("RPC")
}

#[test]
fn main() {
	let bitcoind = init_bitcoind();
	let (xpriv, mut wallet) = init_wallet();

	println!("initial wallet");
	dump_wallet(&wallet);

	// add the tip as first block
	let blockchain_info = bitcoind.get_blockchain_info().expect("RPC");
	wallet.set_last_block(blockchain_info.bestblockhash, blockchain_info.blocks as u32);

	println!("wallet ready");
	dump_wallet(&wallet);

	// receive some txs
	for _ in 0..5 {
		let addr = wallet.new_receive_address();
		bitcoind.send_to_address(&addr, 1.0, None, None, None, None, None, None).expect("RPC");
		let block = generate(&bitcoind);
		wallet.process_block(&block);
	}

	println!("wallet finaly");
	dump_wallet(&wallet);
	println!("balance: {}", wallet.get_balance(None));
	assert_eq!(wallet.get_balance(None), 500000000);

	// make tx
	let delivery_addr = bitcoind.get_new_address(None, None).expect("RPR");
	let output = TxOut{
		value: 250000000,
		script_pubkey: delivery_addr.script_pubkey(),
	};
	let psbt = wallet.create_transaction(vec![output], vec![], 0).expect("create_transaction");
	let b64 = base64::encode(&serialize(&psbt));

	println!("psbt: {}", b64);



}
