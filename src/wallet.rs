use std::collections;
use std::collections::HashMap;

use bitcoin::util::{bip32, psbt};
use bitcoin::{Address, BitcoinHash, Block, OutPoint, Script, Transaction, TxIn, TxOut};
use bitcoin_hashes::sha256d;
use rand::{self, Rng};
use serde::{Deserialize, Serialize};

use config::WalletConfig;
use error::{Error, Result};

#[derive(Serialize, Deserialize)]
pub struct KnownBlock {
	pub height: u32,
	pub hash: sha256d::Hash,
}

#[derive(Clone, Copy)]
pub enum AddressType {
	P2wpkh,
}

impl AddressType {
	pub fn all_types() -> &'static [AddressType] {
		&[AddressType::P2wpkh]
	}
}

/// A UTXO owned by our wallet.
#[derive(Serialize, Deserialize)]
pub struct Utxo {
	pub outpoint: OutPoint,
	pub value: u64,
	pub height: u32,
	/// The child number of the key that is needed to spend this output.
	child_number: bip32::ChildNumber,
}

/// The wallet.
#[derive(Serialize, Deserialize)]
pub struct Wallet {
	config: WalletConfig,

	// address source
	extended_pubkey: bip32::ExtendedPubKey,
	master_fp: bip32::Fingerprint,
	base_derivation_path: bip32::DerivationPath,
	last_sourced_child: Option<bip32::ChildNumber>,

	// UTXOs
	owned_utxos: HashMap<OutPoint, Utxo>,

	// script index
	//TODO(stevenroose) consider mapping based on script hash
	script_index: HashMap<Script, bip32::ChildNumber>,

	// block processing
	last_known_block: Option<KnownBlock>,

	// history
	tx_history: Vec<Transaction>, //TODO(stevenroose) consider hashmap
}

impl Wallet {
	pub fn new(
		config: WalletConfig,
		xpub: bip32::ExtendedPubKey,
		master_fingerprint: bip32::Fingerprint,
		base_path: bip32::DerivationPath,
	) -> Wallet {
		let wallet = Wallet {
			config: config,
			extended_pubkey: xpub,
			master_fp: master_fingerprint,
			base_derivation_path: base_path,
			last_sourced_child: None,
			owned_utxos: HashMap::new(),
			script_index: HashMap::new(),
			last_known_block: None,
			tx_history: Vec::new(),
		};
		wallet
	}

	fn get_history_tx(&self, txid: sha256d::Hash) -> Option<&Transaction> {
		self.tx_history.iter().find(|t| t.txid() == txid)
	}

	fn get_address(&self, idx: bip32::ChildNumber, address_type: AddressType) -> Address {
		let path = self.base_derivation_path.child(idx);
		let xpub = self.extended_pubkey.derive_pub(&::SECP, &path).expect("derivation failure");
		match address_type {
			AddressType::P2wpkh => Address::p2wpkh(&xpub.public_key, self.config.network),
		}
	}

	fn index_script_pubkeys(&mut self, child: bip32::ChildNumber) {
		for address_type in AddressType::all_types() {
			let address = self.get_address(child, *address_type);
			self.script_index.insert(address.script_pubkey(), child);
		}
	}

	/// Increases the wallet's latest address child number and returns it.
	fn next_address_child(&mut self) -> bip32::ChildNumber {
		self.last_sourced_child = Some(match self.last_sourced_child {
			None => bip32::ChildNumber::from_normal_idx(0).unwrap(),
			Some(cn) => cn.increment().expect("BIP32 child number overflow"),
		});
		self.last_sourced_child.unwrap()
	}

	/// Undo the last [next_address_child].
	fn rollback_address_child(&mut self) {
		self.last_sourced_child = Some(match self.last_sourced_child {
			None => bip32::ChildNumber::from_normal_idx(0).unwrap(),
			// manually decrement
			Some(bip32::ChildNumber::Normal {
				index: idx,
			}) => bip32::ChildNumber::from_normal_idx(idx.checked_sub(1).unwrap_or(0)).unwrap(),
			Some(bip32::ChildNumber::Hardened {
				index: idx,
			}) => bip32::ChildNumber::from_hardened_idx(idx.checked_sub(1).unwrap_or(0)).unwrap(),
		});
	}

	pub fn new_receive_address(&mut self) -> Address {
		let idx = self.next_address_child();
		self.index_script_pubkeys(idx);
		self.get_address(idx, AddressType::P2wpkh)
	}

	/// Check if the tx is relevant for the wallet.
	pub fn is_relevant_tx(&self, tx: &Transaction) -> bool {
		tx.input.iter().any(|i| self.owned_utxos.contains_key(&i.previous_output))
			|| tx.output.iter().any(|o| self.script_index.contains_key(&o.script_pubkey))
	}

	fn process_transaction(&mut self, tx: &Transaction, block_height: u32) {
		let mut relevant = false;
		// Find if spending any of our own UTXOs.
		for input in &tx.input {
			if self.owned_utxos.remove(&input.previous_output).is_some() {
				relevant = true;
			}
		}

		// Find if sending to any of our own outputs.
		for (idx, output) in tx.output.iter().enumerate() {
			if let Some(child) = self.script_index.get(&output.script_pubkey) {
				let outpoint = OutPoint {
					txid: tx.txid(),
					vout: idx as u32,
				};

				self.owned_utxos.insert(
					outpoint,
					Utxo {
						outpoint: outpoint,
						value: output.value,
						height: block_height,
						child_number: *child,
					},
				);
				relevant = true;
			}
		}

		if relevant {
			self.tx_history.push(tx.clone());
		}
	}

	pub fn process_block(&mut self, block: &Block, height: u32) -> Result<()> {
		// Ensure the block follows on the last known block.
		if let Some(ref last_block) = self.last_known_block {
			if block.header.prev_blockhash != last_block.hash || height != last_block.height + 1 {
				//TODO(stevenroose) implement reorg logic
				return Err(Error::BlockFork);
			}
		}

		for tx in &block.txdata {
			self.process_transaction(&tx, height)
		}

		self.last_known_block = Some(KnownBlock {
			height: height,
			hash: block.bitcoin_hash(),
		});

		Ok(())
	}

	pub fn get_balance(&self, minimum_confirmations: Option<u32>) -> u64 {
		let current_height = self.last_known_block.as_ref().map(|b| b.height).unwrap_or(0);
		let min_height = match minimum_confirmations {
			None => current_height,
			Some(minconf) => current_height.checked_sub(minconf).unwrap_or(0) + 1,
		};
		let confirmed =
			self.owned_utxos.values().filter(|u| u.height >= min_height).map(|u| u.value).sum();
		//TODO(stevenroose) unconfirmed
		confirmed
	}

	/// Returns an iterator over the [Utxo]s owned by the wallet.
	pub fn get_utxos(&self) -> collections::hash_map::Values<OutPoint, Utxo> {
		self.owned_utxos.values()
	}

	/// returns the change index if there was change
	fn create_transaction_with_change(
		&mut self,
		mut outputs: Vec<TxOut>,
		use_inputs: Vec<OutPoint>,
		change_child: bip32::ChildNumber,
		fee: u64,
	) -> Result<(psbt::PartiallySignedTransaction, Option<usize>)> {
		let mut rng = rand::thread_rng();

		// Check all given inputs.
		let mut total_in = 0;
		let mut in_utxos = HashMap::new();
		for outpoint in &use_inputs {
			if let Some(utxo) = self.owned_utxos.get(outpoint) {
				if in_utxos.insert(outpoint, utxo).is_some() {
					return Err(Error::DuplicateUtxo);
				}
				total_in += utxo.value;
			} else {
				return Err(Error::UtxoNotInWallet);
			}
		}

		// Count the total output value.
		let mut total_out = 0;
		for output in &outputs {
			total_out += output.value;
		}

		// Add random extra inputs from our own UTXOs until sufficient.
		if total_out + fee > total_in {
			// To do this more efficiently, we keep a vector of the
			// remaining UTXOs in the wallet.
			let mut remaining_utxos = Vec::with_capacity(self.owned_utxos.len() - in_utxos.len());
			for outpoint in self.owned_utxos.keys() {
				if !in_utxos.contains_key(outpoint) {
					remaining_utxos.push(outpoint);
				}
			}

			while total_out + fee > total_in {
				if remaining_utxos.is_empty() {
					return Err(Error::InsufficientFunds);
				}

				let rand_idx = rng.gen_range(0, remaining_utxos.len());
				let outpoint = remaining_utxos.remove(rand_idx);
				let utxo = self.owned_utxos.get(outpoint).expect("added ourself above");
				total_in += utxo.value;
				in_utxos.insert(&utxo.outpoint, &utxo);
			}
		}

		// Add change.
		let change_amount = total_in - total_out - fee;
		let change_idx = if change_amount > 0 {
			let change_addr = self.get_address(change_child, AddressType::P2wpkh);
			let change_idx = rng.gen_range(0, outputs.len());
			outputs.insert(
				change_idx,
				TxOut {
					value: change_amount,
					script_pubkey: change_addr.script_pubkey(),
				},
			);
			Some(change_idx)
		} else {
			None
		};

		// Shuffle inputs and prepare PSBT data.
		let mut prevouts: Vec<&OutPoint> = in_utxos.keys().map(|o| *o).collect();
		rng.shuffle(&mut prevouts);
		let mut inputs = vec![];
		let mut psbt_inputs = vec![];
		for prevout in &prevouts {
			let utxo = in_utxos.get(prevout).unwrap();
			inputs.push(TxIn {
				previous_output: *prevout.clone(),
				script_sig: Script::new(),
				sequence: 0xFFFFFFFF,
				witness: vec![],
			});
			psbt_inputs.push(psbt::Input {
				witness_utxo: {
					//TODO(stevenroose) don't assume segwit
					let prev = self.get_history_tx(prevout.txid).expect("missing history");
					assert!(prevout.vout < prev.output.len() as u32);
					Some(prev.output[prevout.vout as usize].clone())
				},
				hd_keypaths: {
					let path = self.base_derivation_path.child(utxo.child_number);
					let pubkey = self.extended_pubkey.derive_pub(&::SECP, &path)?.public_key;
					let mut ret = HashMap::new();
					ret.insert(pubkey, (self.master_fp, path));
					ret
				},
				..Default::default()
			});
		}
		// PSBT output for change.
		let mut psbt_outputs: Vec<psbt::Output> = vec![Default::default(); outputs.len()];
		if let Some(idx) = change_idx {
			let path = self.base_derivation_path.child(change_child);
			let pubkey = self.extended_pubkey.derive_pub(&::SECP, &path)?.public_key;
			psbt_outputs[idx].hd_keypaths.insert(pubkey, (self.master_fp, path));
		}

		// Create the unsigned tx.  Shuffle inputs and outputs.
		let tx = Transaction {
			version: 1,
			lock_time: 0, //TODO(stevenroose)
			input: inputs,

			output: outputs,
		};

		Ok((
			psbt::PartiallySignedTransaction {
				global: psbt::Global::from_unsigned_tx(tx).expect("only when non-empty sigs"),
				inputs: psbt_inputs,
				outputs: psbt_outputs,
			},
			change_idx,
		))
	}

	///
	/// Possible errors:
	/// - [Error::Bip32]
	/// - [Error::DuplicateUtxo]
	/// - [Error::InsufficientFunds]
	/// - [Error::UtxoNotInWallet]
	pub fn create_transaction(
		&mut self,
		outputs: Vec<TxOut>,
		use_inputs: Vec<OutPoint>,
		fee: u64,
	) -> Result<psbt::PartiallySignedTransaction> {
		let change_child = self.next_address_child();
		let (psbt, change_idx) =
			self.create_transaction_with_change(outputs, use_inputs, change_child, fee)?;
		if change_idx.is_none() {
			self.rollback_address_child();
		}
		Ok(psbt)
	}
}
