use anyhow::Result;
use bdk::bitcoin::util::bip32::ExtendedPubKey;
use bdk::bitcoin::{self, util::bip32::ExtendedPrivKey};
use bip39::Seed;

pub struct Keychain {
    seed: Seed,
}

impl Keychain {
    pub fn new(seed: Seed) -> Self {
        Keychain { seed }
    }

    pub fn private_key(&self, network: bitcoin::Network) -> Result<ExtendedPrivKey> {
        Ok(ExtendedPrivKey::new_master(network, self.seed.as_bytes()).unwrap())
    }
}
