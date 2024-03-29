use std::collections::{HashMap, HashSet};
use std::{collections, fmt};

use bitcoin::util::{bip32, psbt};
use bitcoin::{Address, BitcoinHash, Block, OutPoint, Script, Transaction, TxIn, TxOut};
use bitcoin_hashes::sha256d;
use rand::{self, Rng};
use serde::{Deserialize, Serialize};

use config::WalletConfig;
use error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownBlock {
	pub height: u32,
	pub hash: sha256d::Hash,
}

#[derive(Debug, Clone, Copy)]
pub enum AddressType {
	P2wpkh,
}

impl AddressType {
	pub fn all_types() -> &'static [AddressType] {
		&[AddressType::P2wpkh]
	}
}

/// A UTXO owned by our wallet.
#[derive(Debug, Serialize, Deserialize)]
pub struct Utxo {
	pub outpoint: OutPoint,
	pub value: u64,
	pub height: u32,

	/// The child number of the key that is needed to spend this output.
	child_number: bip32::ChildNumber,

	/// This UTXO has been used in the following txs.
	used_in_tx: HashSet<sha256d::Hash>,
}

impl Utxo {
	pub fn is_available(&self) -> bool {
		self.used_in_tx.is_empty()
	}
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

	// ongoing and mempool
	pending_txs: Vec<Transaction>,

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
			pending_txs: Vec::new(),
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
						used_in_tx: HashSet::new(),
					},
				);
				relevant = true;
			}
		}

		if relevant {
			self.tx_history.push(tx.clone());
		}
	}

	/// Use this only when you know what you are doing. This might make the wallet lose track of
	/// some of its own UTXOs.
	pub fn set_last_block(&mut self, block_hash: sha256d::Hash, height: u32) {
		self.last_known_block = Some(KnownBlock {
			hash: block_hash,
			height: height,
		});
	}

	pub fn process_block(&mut self, block: &Block) -> Result<()> {
		if self.last_known_block.is_none() {
			return Err(Error::WalletNotFullyInitialized);
		}

		// Ensure the block follows on the last known block.
		if block.header.prev_blockhash != self.last_known_block.as_ref().unwrap().hash {
			//TODO(stevenroose) implement reorg logic
			return Err(Error::BlockFork);
		}
		let new_height = self.last_known_block.as_ref().unwrap().height + 1;

		for tx in &block.txdata {
			self.process_transaction(&tx, new_height)
		}

		self.last_known_block = Some(KnownBlock {
			height: new_height,
			hash: block.bitcoin_hash(),
		});

		Ok(())
	}

	pub fn get_balance(&self, minimum_confirmations: Option<u32>) -> u64 {
		let current_height = self.last_known_block.as_ref().map(|b| b.height).unwrap_or(0);
		let max_height = match minimum_confirmations {
			None => current_height,
			Some(minconf) => current_height.checked_sub(minconf).unwrap_or(0) + 1,
		};
		let confirmed =
			self.owned_utxos.values().filter(|u| u.height <= max_height).map(|u| u.value).sum();
		//TODO(stevenroose) unconfirmed
		confirmed
	}

	/// Returns an iterator over the [Utxo]s owned by the wallet.
	pub fn get_utxos(&self) -> collections::hash_map::Values<OutPoint, Utxo> {
		self.owned_utxos.values()
	}

	/// Commit to the tx by considering the UTXOs it spends as used in the tx.
	/// The tx will also be kept as pending.
	/// No check is done to prevent adding the same tx twice.
	pub fn commit_transaction(&mut self, tx: Transaction) {
		let txid = tx.txid();
		for input in &tx.input {
			if let Some(utxo) = self.owned_utxos.get_mut(&input.previous_output) {
				utxo.used_in_tx.insert(txid);
			}
		}
		self.pending_txs.push(tx);
	}

	/// Drop a transaction that is considered pending by the wallet.
	/// This also frees the UTXOs the transaction was spending to being used
	/// again in new txs.
	pub fn drop_pending_transaction(&mut self, txid: sha256d::Hash) -> bool {
		for (_, utxo) in self.owned_utxos.iter_mut() {
			utxo.used_in_tx.remove(&txid);
		}

		let len_before = self.pending_txs.len();
		self.pending_txs.retain(|tx| tx.txid() != txid);
		self.pending_txs.len() < len_before
	}

	/// - Returns the change index if there was change.
	/// - This method does not commit the tx inputs.
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
			for (outpoint, utxo) in self.owned_utxos.iter() {
				if !in_utxos.contains_key(outpoint) && utxo.is_available() {
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

		// Create the unsigned tx.
		let tx = Transaction {
			version: 1,
			lock_time: 0,
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
			match self.create_transaction_with_change(outputs, use_inputs, change_child, fee) {
				Ok(res) => res,
				Err(e) => {
					self.rollback_address_child();
					return Err(e);
				}
			};
		if change_idx.is_none() {
			self.rollback_address_child();
		}
		self.commit_transaction(psbt.global.unsigned_tx.clone());
		Ok(psbt)
	}
}

impl fmt::Debug for Wallet {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		use bitcoin_hashes::hex::ToHex;

		write!(f, "--- Wallet ---\n")?;
		write!(f, "config: {:?}\n", self.config)?;
		write!(f, "extended_pubkey: {}\n", self.extended_pubkey)?;
		write!(f, "master_fp: {}\n", self.master_fp[..].to_hex())?;
		write!(f, "base_derivation_path: {}\n", self.base_derivation_path)?;
		write!(f, "last_sourced_child: {:?}\n", self.last_sourced_child)?;
		write!(f, "owned_utxos (len: {}):\n", self.owned_utxos.len())?;
		for utxo in self.owned_utxos.values() {
			write!(f, "- {:?}\n", utxo)?;
		}
		write!(f, "script_index (len: {}):\n", self.script_index.len())?;
		for (script, cn) in self.script_index.iter() {
			write!(f, "- {}: {}\n", script.to_hex(), cn)?;
		}
		write!(f, "last_known_block: {:?}\n", self.last_known_block)?;
		write!(f, "pending_txs (len: {}):\n", self.pending_txs.len())?;
		for tx in self.pending_txs.iter() {
			write!(f, "- {:?}\n", tx)?;
		}
		write!(f, "tx_history (len: {}):\n", self.tx_history.len())?;
		for tx in self.tx_history.iter() {
			write!(f, "- {:?}\n", tx)?;
		}
		write!(f, "--------------")
	}
}
