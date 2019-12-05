use bip39::{Language, Mnemonic};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use super::guard::KeystoreGuard;
use super::Address;
use super::Result;
use super::{Account, Extra};
use super::{Error, Metadata, Source};

use crate::keystore::{Keystore, Store};

use core::{fmt, result};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::str::FromStr;
use tcx_constants::{CoinInfo, CurveType};
use tcx_crypto::{Crypto, Pbkdf2Params};
use tcx_primitive::{
    generate_mnemonic, get_account_path, Derive, DerivePath, DeterministicType,
    TypedDeterministicPrivateKey, TypedDeterministicPublicKey, TypedPrivateKey,
};

struct Cache {
    mnemonic: String,
    seed: Vec<u8>,
    keys: HashMap<String, TypedDeterministicPrivateKey>,
}

pub struct HdKeystore {
    store: Store,
    cache: Option<Cache>,
}

impl HdKeystore {
    pub const VERSION: i64 = 11000i64;

    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    pub(crate) fn from_store(store: Store) -> Self {
        HdKeystore { store, cache: None }
    }

    pub fn unlock_by_password(&mut self, password: &str) -> Result<()> {
        let mnemonic_bytes = self.store.crypto.decrypt(password)?;
        let mnemonic_str = String::from_utf8(mnemonic_bytes)?;

        let mnemonic = Mnemonic::from_phrase(&mnemonic_str, Language::English)
            .map_err(|_| Error::InvalidMnemonic)?;

        self.cache = Some(Cache {
            mnemonic: mnemonic_str,
            seed: bip39::Seed::new(&mnemonic, &"").as_bytes().to_vec(),
            keys: HashMap::new(),
        });

        Ok(())
    }

    pub fn lock(&mut self) {
        self.cache = None;
    }

    pub fn mnemonic(&self) -> Result<&str> {
        let cache = self.cache.as_ref().ok_or(Error::KeystoreLocked)?;

        Ok(&cache.mnemonic)
    }

    pub fn seed(&self) -> Result<&Vec<u8>> {
        let cache = self.cache.as_ref().ok_or(Error::KeystoreLocked)?;

        Ok(&cache.seed)
    }

    pub fn find_private_key(&self, symbol: &str, address: &str) -> Result<TypedPrivateKey> {
        let cache = self.cache.as_ref().ok_or(Error::KeystoreLocked)?;

        let account = self
            .account(symbol, address)
            .ok_or(Error::AccountNotFound)?;

        let root = TypedDeterministicPrivateKey::from_seed(
            DeterministicType::BIP32,
            account.curve,
            &cache.seed,
        )?;

        Ok(root
            .derive(DerivePath::from_str(&account.derivation_path)?.into_iter())?
            .private_key())
    }

    pub fn find_private_key_by_path(
        &mut self,
        symbol: &str,
        address: &str,
        path: &str,
    ) -> Result<TypedPrivateKey> {
        let cache = self.cache.as_ref().ok_or(Error::KeystoreLocked)?;

        if !cache.keys.contains_key(address) {
            let account = self
                .account(symbol, address)
                .ok_or(Error::AccountNotFound)?;

            let esk = TypedDeterministicPrivateKey::from_seed(
                DeterministicType::BIP32,
                account.curve,
                &cache.seed,
            )?;

            let k = esk.derive(
                DerivePath::from_str(&get_account_path(&account.derivation_path)?)?.into_iter(),
            )?;

            self.cache
                .as_mut()
                .unwrap()
                .keys
                .insert(address.to_owned(), k);
        }

        let esk = &self.cache.as_ref().unwrap().keys[address];

        Ok(esk
            .derive(DerivePath::from_str(path)?.into_iter())?
            .private_key())
    }

    pub fn new(password: &str, meta: Metadata) -> HdKeystore {
        let mnemonic = generate_mnemonic();
        let crypto: Crypto<Pbkdf2Params> = Crypto::new(password, mnemonic.as_bytes());
        HdKeystore {
            store: Store {
                id: Uuid::new_v4().to_hyphenated().to_string(),
                version: Self::VERSION,
                crypto,
                active_accounts: vec![],
                meta,
            },

            cache: None,
        }
    }

    pub fn from_mnemonic(mnemonic: &str, password: &str, meta: Metadata) -> HdKeystore {
        let crypto: Crypto<Pbkdf2Params> = Crypto::new(password, mnemonic.as_bytes());
        HdKeystore {
            store: Store {
                id: Uuid::new_v4().to_hyphenated().to_string(),
                version: Self::VERSION,
                crypto,
                active_accounts: vec![],
                meta,
            },

            cache: None,
        }
    }

    pub fn derive_coin<A: Address, E: Extra>(&mut self, coin_info: &CoinInfo) -> Result<&Account> {
        let mut cache = self.cache.as_mut().ok_or(Error::KeystoreLocked)?;

        let root = TypedDeterministicPrivateKey::from_seed(
            DeterministicType::BIP32,
            coin_info.curve,
            &cache.seed,
        )?;

        let private_key = root
            .derive(DerivePath::from_str(&coin_info.derivation_path)?.into_iter())?
            .private_key();
        let public_key = private_key.public_key();

        let address = A::from_public_key(&public_key, coin_info)?;

        let account = Account {
            address,
            derivation_path: coin_info.derivation_path.clone(),
            curve: coin_info.curve,
            coin: coin_info.symbol.clone(),
            extra: Default::default(),
        };

        self.store.active_accounts.push(account);

        Ok(self.store.active_accounts.last().unwrap())
    }

    pub fn account(&self, symbol: &str, address: &str) -> Option<&Account> {
        self.store
            .active_accounts
            .iter()
            .find(|acc| acc.address == address && acc.coin == symbol)
    }

    pub fn verify_password(&self, password: &str) -> bool {
        self.store.crypto.verify_password(password)
    }
}

fn merge_value(a: &mut Value, b: &Value) {
    match (a, b) {
        (&mut Value::Object(ref mut a), &Value::Object(ref b)) => {
            for (k, v) in b {
                merge_value(a.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (a, b) => {
            *a = b.clone();
        }
    }
}

/*
impl Display for HdKeystore {
    fn fmt(&self, f: &mut Formatter<'_>) -> result::Result<(), fmt::Error> {
        let mut pw = Map::new();
        pw.insert("id".to_string(), json!(&self.id.to_string()));
        pw.insert("name".to_string(), json!(&self.meta.name));
        pw.insert("passwordHint".to_string(), json!(&self.meta.password_hint));
        pw.insert("createdAt".to_string(), json!(&self.meta.timestamp));
        pw.insert("source".to_string(), json!(&self.meta.source));

        if !&self.active_accounts.is_empty() {
            if self.active_accounts.len() > 1usize {
                panic!("Only one account in token 2.5");
            }
            let acc = &self
                .active_accounts
                .first()
                .expect("get first account from hdkeystore");
            pw.insert("address".to_string(), json!(acc.address.to_string()));
            let coin_split: Vec<&str> = acc.coin.split('-').collect();
            coin_split.iter().enumerate().for_each(|(i, s)| {
                if i == 0 {
                    pw.insert("chainType".to_string(), json!(s));
                } else if vec!["NONE", "P2WPKH"].contains(s) {
                    pw.insert("segWit".to_string(), json!(s));
                }
            });
            let mut obj = Value::Object(pw);
            if let Some(extra) = acc.extra.as_object() {
                merge_value(&mut obj, &Value::Object(extra.clone()))
            }
            write!(
                f,
                "{}",
                serde_json::to_string(&obj).expect("present err when convert to json")
            )
        } else {
            write!(
                f,
                "{}",
                serde_json::to_string(&pw).expect("present err when convert to json")
            )
        }
    }
}
*/

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystore::{metadata_default_time, EmptyExtra};
    use bitcoin_hashes::hex::ToHex;
    use serde_json::Map;
    use tcx_primitive::{PublicKey, TypedPublicKey};

    static PASSWORD: &'static str = "Insecure Pa55w0rd";
    static MNEMONIC: &'static str =
        "inject kidney empty canal shadow pact comfort wife crush horse wife sketch";

    #[test]
    pub fn default_meta_test() {
        let meta = Metadata::default();
        let expected = Metadata {
            name: String::from("Unknown"),
            password_hint: String::new(),
            timestamp: metadata_default_time(),
            source: Source::Mnemonic,
        };

        assert_eq!(meta.name, expected.name);
        assert_eq!(meta.password_hint, expected.password_hint);
        assert_eq!(meta.source, expected.source);
    }

    struct MockAddress {}
    impl Address for MockAddress {
        fn from_public_key(_pk: &TypedPublicKey, _coin: &CoinInfo) -> Result<String> {
            Ok("mock_address".to_string())
        }

        fn is_valid(address: &str) -> bool {
            true
        }
    }

    #[test]
    pub fn mnemonic_to_account_test() {
        /*
        let coin_info = CoinInfo {
            symbol: "BITCOINCASH".to_string(),
            derivation_path: "m/44'/0'/0'/0/0".to_string(),
            curve: CurveType::SECP256k1,
        };
        let account =
            HdKeystore::mnemonic_to_account::<MockAddress, EmptyExtra>(&coin_info, MNEMONIC)
                .unwrap();
        let expected = Account {
            address: "mock_address".to_string(),
            derivation_path: "m/44'/0'/0'/0/0".to_string(),
            curve: CurveType::SECP256k1,
            coin: "BITCOINCASH".to_string(),
            extra: Value::Object(Map::new()),
        };
        assert_eq!(account, expected);
        */
    }

    #[test]
    pub fn new_keystore() {
        let keystore = HdKeystore::new(PASSWORD, Metadata::default());
        let store = keystore.store;
        assert_eq!(store.version, 11000);
        assert_ne!(store.id, "");
        assert_eq!(store.active_accounts.len(), 0);
    }

    #[test]
    pub fn from_mnemonic_test() {
        let mut keystore = HdKeystore::from_mnemonic(MNEMONIC, PASSWORD, Metadata::default());
        assert_eq!(keystore.store.version, 11000);
        assert_ne!(keystore.store.id, "");
        let decrypted_bytes = keystore.store.crypto.decrypt(PASSWORD).unwrap();
        let decrypted_mnemonic = String::from_utf8(decrypted_bytes).unwrap();
        assert_eq!(decrypted_mnemonic, MNEMONIC);
        assert_eq!(keystore.store.active_accounts.len(), 0);

        keystore.unlock_by_password(PASSWORD);

        let mnemonic = keystore.mnemonic().unwrap();
        assert_eq!(mnemonic, MNEMONIC);

        let expected_seed = "ee3fce3ccf05a2b58c851e321077a63ee2113235112a16fc783dc16279ff818a549ff735ac4406c624235db2d37108e34c6cbe853cbe09eb9e2369e6dd1c5aaa";

        let seed = keystore.seed().unwrap();
        assert_eq!(seed.to_hex(), expected_seed);

        let wrong_password_err = keystore.unlock_by_password("WrongPassword").err().unwrap();
        assert_eq!(format!("{}", wrong_password_err), "password_incorrect");
    }

    //
    //    #[test]
    //    pub fn get_pair_test() {
    //        let keystore = HdKeystore::from_mnemonic(MNEMONIC, PASSWORD, Metadata::default());
    //        let pair = keystore
    //            .get_pair::<tcx_primitive::key::secp256k1::Pair>("m/44'/0'/0'", PASSWORD)
    //            .unwrap();
    //        let xpub = pair.to_string();
    //        assert_eq!(xpub, "xprv9yrdwPSRnvomqFK4u1y5uW2SaXS2Vnr3pAYTjJjbyRZR8p9BwoadRsCxtgUFdAKeRPbwvGRcCSYMV69nNK4N2kadevJ6L5iQVy1SwGKDTHQ");
    //    }

    #[test]
    pub fn derive_key_at_paths_test() {
        let mut keystore = HdKeystore::from_mnemonic(MNEMONIC, PASSWORD, Metadata::default());
        let coin_info = CoinInfo {
            symbol: "BITCOIN".to_string(),
            derivation_path: "m/44'/0'/0'/0/0".to_string(),
            curve: CurveType::SECP256k1,
        };
        let _ = keystore.unlock_by_password(PASSWORD);

        let acc = keystore
            .derive_coin::<MockAddress, EmptyExtra>(&coin_info)
            .unwrap();
        let expected = Account {
            address: "mock_address".to_string(),
            derivation_path: "m/44'/0'/0'/0/0".to_string(),
            curve: CurveType::SECP256k1,
            coin: "BITCOIN".to_string(),
            extra: Value::Object(Map::new()),
        };

        assert_eq!(acc, &expected);
        assert_eq!(keystore.account("BITCOIN", "").unwrap(), &expected);
        assert_eq!(keystore.store.active_accounts.len(), 1);

        let paths = vec![
            "m/44'/0'/0'/0/0",
            "m/44'/0'/0'/0/1",
            "m/44'/0'/0'/1/0",
            "m/44'/0'/0'/1/1",
        ];

        let _ = keystore.unlock_by_password(PASSWORD);
        /*
        let private_keys = keystore.key_at_paths("BITCOIN", &paths).unwrap();
        let pub_keys = private_keys
            .iter()
            .map(|epk| epk.private_key().public_key().to_bytes().to_hex())
            .collect::<Vec<String>>();
        let expected_pub_keys = vec![
            "026b5b6a9d041bc5187e0b34f9e496436c7bff261c6c1b5f3c06b433c61394b868",
            "024fb7df3961e08f01025e434ea19708a4317d2fe59775cddd38df6e8a2d30697d",
            "0352470ace48f25b01b9c341e3b0e033fc32a203fb7a81a0453f97d94eca819a35",
            "022f4c38f7bbaa00fc886db62f975b34201c2bfed146e98973caf03268941801db",
        ];
        assert_eq!(pub_keys, expected_pub_keys);
        */
    }
}
