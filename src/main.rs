use anyhow::Result;
use bdk::bitcoin::hashes::hex::ToHex;
use bdk::bitcoin::secp256k1::Secp256k1;
use bdk::bitcoin::util::bip32::ExtendedPubKey;
use bdk::bitcoin::Address;
use bdk::blockchain::esplora::EsploraBlockchainConfig;
use bdk::blockchain::{
    log_progress, noop_progress, AnyBlockchain, AnyBlockchainConfig, ConfigurableBlockchain,
    ElectrumBlockchainConfig,
};
use bdk::database::MemoryDatabase;
use bdk::descriptor::policy::SatisfiableItem;
use bdk::descriptor::ExtendedDescriptor;
use bdk::keys::{DescriptorSecretKey, IntoDescriptorKey};
use bdk::miniscript::descriptor::{DescriptorXKey, KeyMap};
use bdk::miniscript::policy::Concrete;
use bdk::miniscript::{Descriptor, DescriptorPublicKey};
use bdk::{sled, SignOptions, KeychainKind};
use bdk::wallet::tx_builder::{ChangeSpendPolicy, TxOrdering};
use bdk::{
    descriptor::template::DescriptorTemplate, FeeRate, TxBuilder, Wallet,
};
use bip39::{Language, MnemonicType, Seed};
use std::collections::BTreeMap;
use std::iter::FromIterator;
use std::process;
use std::str::FromStr;
use structopt::StructOpt;
use bdk::wallet::AddressIndex::New;

mod descriptor;
mod keychain;

const NETWORK: bdk::bitcoin::Network = bdk::bitcoin::Network::Regtest;

#[derive(StructOpt)]
#[structopt(name = "dms", about = "A dead man's switch for bitcoin.")]
enum DeadMansSwitch {
    /// Create a wallet with a mnemonic seed (or use an existing one)
    /// and return descriptors to move and redeem funds.
    Create {
        /// Mnemonic seed to restore from.
        mnemonic: Option<String>,

        /// A passphrase for check-in/withdraw key (owner).
        #[structopt(long)]
        owner_passphrase: String,

        /// A passphrase for redeem key (redeemer).
        #[structopt(long)]
        redeem_passphrase: String,
    },

    /// Move funds to the next address to show that the owner is alive.
    /// This should be done before a redeem time-lock expires.
    CheckIn {
        /// A wallet's descriptor from the `create` command. Includes private keys.
        #[structopt(long, env = "DESCRIPTOR")]
        descriptor: String,

        /// An Electrum server URL.
        #[structopt(long)]
        electrum: String,
    },

    /// Stop using this wallet and move all of the funds to the specified bitcoin address.
    Withdraw {
        /// A destination bitcoin address.
        address: String,

        /// A passphrase for check-in/withdraw key (owner).
        #[structopt(long)]
        owner_passphrase: String,
    },

    /// Redeem locked funds. Could only be done after a time-lock is expired.
    Redeem {
        /// A passphrase for redeem key (redeemer).
        #[structopt(long)]
        redeem_passphrase: String,
    },
}

fn main() -> anyhow::Result<()> {
    let options = DeadMansSwitch::from_args();

    match options {
        DeadMansSwitch::Create {
            owner_passphrase,
            redeem_passphrase,
            mnemonic,
        } => {
            let (_, keys_redeemer) = create_redeemer(&mnemonic, &redeem_passphrase)?;
            let (_, redeemer_xprv) = keys_redeemer.into_iter().nth(0).unwrap();

            let stash_desc = create_stash(redeemer_xprv, &mnemonic, &owner_passphrase)?;
            println!("[stash] Save this descriptor: {}", stash_desc);

            let database = MemoryDatabase::new();
            let wallet = Wallet::new_offline(&stash_desc, None, NETWORK, database)?;
            dbg!(wallet.get_address(New).expect("new addr"));
        }

        DeadMansSwitch::CheckIn {
            descriptor,
            electrum,
        } => {
            // let database = sled::open(".wallet_data")?;
            // let database = database.open_tree("dms")?;
            let database = MemoryDatabase::new();
            let config = AnyBlockchainConfig::Electrum(ElectrumBlockchainConfig {
                url: electrum,
                socks5: None,
                retry: 10,
                timeout: Some(100),
            });
            let client = AnyBlockchain::from_config(&config)?;
            let wallet = Wallet::new(&descriptor, None, NETWORK, database, client)?;
            wallet.sync(log_progress(), None)?;

            let new_addr = wallet.get_address(New)?;
            let balance = wallet.get_balance()?;
            if balance == 0 {
                println!(
                    "[!] Balance is empty. Please deposit some funds to this address: {}",
                    new_addr
                );
                process::exit(1);
            }

            println!("Balance is {} sats", balance);

            // There should be a single UTXO which is our stash
            let utxos = wallet.list_unspent()?;
            let old_addresses = utxos
                .into_iter()
                .map(|utxo| {
                    (
                        Address::from_script(&utxo.txout.script_pubkey, NETWORK),
                        utxo.txout.value,
                    )
                })
                .collect::<Vec<_>>();

            println!("[ Moving Funds ]");
            for (addr, sats) in old_addresses {
                println!("From:\t{} ({} sats)", addr.unwrap(), sats);
            }
            println!("To:\t{}", new_addr);

            // Choose "check-in" policy that only requires an owner's signature
            let policy_path = wallet.policies(KeychainKind::External)?.unwrap();
            let policy_path = BTreeMap::from_iter(vec![(policy_path.id, vec![1])]);

            // Create move TX
            let (mut psbt, _) = {
                let mut builder = wallet.build_tx();
                builder
                    .drain_wallet()
                    .set_single_recipient(new_addr.script_pubkey())
                    .fee_rate(FeeRate::from_sat_per_vb(5.0))
                    .policy_path(policy_path, KeychainKind::External);
                builder.finish()?
            };
            let finalized = wallet.sign(&mut psbt, SignOptions::default())?;

            if !finalized {
                eprintln!("Unable to finalize transaction");
                process::exit(1);
            } else {
                println!("[!] Broadcasting transaction...");

                let tx = psbt.extract_tx();
                //let txid = wallet.broadcast(tx)?;

                //println!("[!] Txid: {}", txid);
            }
        }

        _ => unimplemented!(),
    }

    Ok(())
}

fn create_redeemer(mnemonic: &Option<String>, passphrase: &str) -> Result<(String, KeyMap)> {
    let is_mnemonic_generated = mnemonic.is_none();
    let mnemonic = match mnemonic {
        Some(mnemonic) => bip39::Mnemonic::from_phrase(&mnemonic, Language::English)?,
        None => bip39::Mnemonic::new(MnemonicType::Words12, Language::English),
    };

    let key = keychain::Keychain::new(Seed::new(&mnemonic, passphrase));
    let xpriv = key.private_key(NETWORK)?;

    if is_mnemonic_generated {
        let phrase = mnemonic.into_phrase();
        // TODO: better display for mnemonic
        println!("[redeemer] Generated mnemonic seed: {}", phrase);
    }

    let (desc, keymap, _) =
        bdk::descriptor::template::Bip84(xpriv, KeychainKind::External).build()?;
    Ok((desc.to_string_with_secret(&keymap), keymap))
}

fn create_stash(
    redeemer_xprv: DescriptorSecretKey,
    mnemonic: &Option<String>,
    passphrase: &str,
) -> Result<String> {
    let is_mnemonic_generated = mnemonic.is_none();
    let mnemonic = match mnemonic {
        Some(mnemonic) => bip39::Mnemonic::from_phrase(&mnemonic, Language::English)?,
        None => bip39::Mnemonic::new(MnemonicType::Words12, Language::English),
    };

    let key = keychain::Keychain::new(Seed::new(&mnemonic, passphrase));
    let xpriv = key.private_key(NETWORK)?;

    if is_mnemonic_generated {
        let phrase = mnemonic.into_phrase();
        // TODO: better display for mnemonic
        println!("[redeemer] Generated mnemonic seed: {}", phrase);
    }

    let (_, keymap, _) = bdk::descriptor::template::Bip84(xpriv, KeychainKind::External).build()?;
    let (_, desc_xprv) = keymap.into_iter().nth(0).unwrap();

    let (desc, keymap, _) =
        descriptor::MoveOrRedeemWithTimeLock::new(desc_xprv, redeemer_xprv).build()?;
    let desc_str = desc.to_string_with_secret(&keymap);
    Ok(desc_str)
}
