// Rust Bitcoin Wallet
// Written in 2019 by
//   Steven Roose <steven@stevenroose.org>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the CC0 Public Domain Dedication
// along with this software.
// If not, see <http://creativecommons.org/publicdomain/zero/1.0/>.
//

//! # Rust Bitcoin Wallet
//!
//!

#![crate_name = "bitcoin_wallet"]
#![crate_type = "dylib"]
#![crate_type = "rlib"]

// Coding conventions
#![forbid(unsafe_code)]
#![deny(non_upper_case_globals)]
#![deny(non_camel_case_types)]
#![deny(non_snake_case)]
#![deny(unused_mut)]

extern crate bitcoin;
extern crate bitcoin_hashes;
extern crate byteorder;
extern crate hex;
#[macro_use]
extern crate lazy_static;
extern crate rand;
extern crate secp256k1;
extern crate serde;

#[cfg(feature="bitcoinconsensus")] extern crate bitcoinconsensus;

pub mod config;
pub mod error;
pub mod wallet;


lazy_static! {
    static ref SECP: secp256k1::Secp256k1<secp256k1::All> = secp256k1::Secp256k1::new();
}
