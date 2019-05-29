
use std::{fmt, error, result};

use bitcoin::util::bip32;
use secp256k1;


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
	Bip32(bip32::Error),
	Secp256k1(secp256k1::Error),
	BlockFork,
	UtxoNotInWallet,
	DuplicateUtxo,
	InsufficientFunds,
	WalletNotFullyInitialized,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let desc = error::Error::description;
        match *self {
			Error::Bip32(ref e) => write!(f, "{}: {}", desc(self), e),
			Error::Secp256k1(ref e) => write!(f, "{}: {}", desc(self), e),
			_ => f.write_str(desc(self)),
        }
    }
}

impl error::Error for Error {
    fn cause(&self) -> Option<&error::Error> {
        match *self {
			Error::Bip32(ref e) => Some(e),
			Error::Secp256k1(ref e) => Some(e),
			_ => None,
		}
    }

    fn description(&self) -> &str {
        match *self {
			Error::Bip32(_) => "BIP-32 error",
			Error::Secp256k1(_) => "secp256k1 error",
			Error::BlockFork => "block forks off the last known block",
			Error::UtxoNotInWallet => "a UTXO was used that is not part of the wallet",
			Error::DuplicateUtxo => "a UTXO has been provided more than once",
			Error::InsufficientFunds => "not enough funds to fund the given transaction",
			Error::WalletNotFullyInitialized => "the wallet is not fully initialized yet",
        }
    }
}

impl From<secp256k1::Error> for Error {
	fn from(e: secp256k1::Error) -> Error {
		Error::Secp256k1(e)
	}
}

impl From<bip32::Error> for Error {
	fn from(e: bip32::Error) -> Error {
		Error::Bip32(e)
	}
}

pub type Result<T> = result::Result<T, Error>;
