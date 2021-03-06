// Copyright 2019 Contributors to the Parsec project.
// SPDX-License-Identifier: Apache-2.0
use super::Provide;
use crate::authenticators::ApplicationName;
use crate::key_info_managers::{KeyTriple, ManageKeyInfo};
use constants::PSA_SUCCESS;
use derivative::Derivative;
use log::error;
use parsec_interface::operations::list_providers::ProviderInfo;
use parsec_interface::operations::{
    psa_destroy_key, psa_export_public_key, psa_generate_key, psa_import_key, psa_sign_hash,
    psa_verify_hash,
};
use parsec_interface::requests::{Opcode, ProviderID, ResponseStatus, Result};
use psa_crypto_binding::psa_key_id_t;
use std::collections::HashSet;
use std::io::{Error, ErrorKind};
use std::sync::{Arc, Mutex, RwLock};
use std_semaphore::Semaphore;
use utils::KeyHandle;
use uuid::Uuid;

#[allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code,
    trivial_casts
)]
#[allow(clippy::all)]
mod psa_crypto_binding {
    include!(concat!(env!("OUT_DIR"), "/psa_crypto_bindings.rs"));
}

mod asym_sign;
#[allow(dead_code)]
mod constants;
mod key_management;
mod utils;

type LocalIdStore = HashSet<psa_key_id_t>;

const SUPPORTED_OPCODES: [Opcode; 6] = [
    Opcode::PsaGenerateKey,
    Opcode::PsaDestroyKey,
    Opcode::PsaSignHash,
    Opcode::PsaVerifyHash,
    Opcode::PsaImportKey,
    Opcode::PsaExportPublicKey,
];

#[derive(Derivative)]
#[derivative(Debug)]
pub struct MbedProvider {
    // When calling write on a reference of key_info_store, a type
    // std::sync::RwLockWriteGuard<dyn ManageKeyInfo + Send + Sync> is returned. We need to use the
    // dereference operator (*) to access the inner type dyn ManageKeyInfo + Send + Sync and then
    // reference it to match with the method prototypes.
    #[derivative(Debug = "ignore")]
    key_info_store: Arc<RwLock<dyn ManageKeyInfo + Send + Sync>>,
    local_ids: RwLock<LocalIdStore>,
    // Calls to `psa_open_key`, `psa_generate_key` and `psa_destroy_key` are not thread safe - the slot
    // allocation mechanism in Mbed Crypto can return the same key slot for overlapping calls.
    // `key_handle_mutex` is use as a way of securing access to said operations among the threads.
    // This issue tracks progress on fixing the original problem in Mbed Crypto:
    // https://github.com/ARMmbed/mbed-crypto/issues/266
    key_handle_mutex: Mutex<()>,
    // As mentioned above, calls dealing with key slot allocation are not secured for concurrency.
    // `key_slot_semaphore` is used to ensure that only `PSA_KEY_SLOT_COUNT` threads can have slots
    // assigned at any time.
    #[derivative(Debug = "ignore")]
    key_slot_semaphore: Semaphore,
}

impl MbedProvider {
    /// Creates and initialise a new instance of MbedProvider.
    /// Checks if there are not more keys stored in the Key Info Manager than in the MbedProvider and
    /// if there, delete them. Adds Key IDs currently in use in the local IDs store.
    /// Returns `None` if the initialisation failed.
    fn new(key_info_store: Arc<RwLock<dyn ManageKeyInfo + Send + Sync>>) -> Option<MbedProvider> {
        // Safety: this function should be called before any of the other Mbed Crypto functions
        // are.
        if unsafe { psa_crypto_binding::psa_crypto_init() } != PSA_SUCCESS {
            error!("Error when initialising Mbed Crypto");
            return None;
        }
        let mbed_provider = MbedProvider {
            key_info_store,
            local_ids: RwLock::new(HashSet::new()),
            key_handle_mutex: Mutex::new(()),
            key_slot_semaphore: Semaphore::new(constants::PSA_KEY_SLOT_COUNT),
        };
        {
            // The local scope allows to drop store_handle and local_ids_handle in order to return
            // the mbed_provider.
            let mut store_handle = mbed_provider
                .key_info_store
                .write()
                .expect("Key store lock poisoned");
            let mut local_ids_handle = mbed_provider
                .local_ids
                .write()
                .expect("Local ID lock poisoned");
            let mut to_remove: Vec<KeyTriple> = Vec::new();
            // Go through all MbedProvider key triple to key info mappings and check if they are still
            // present.
            // Delete those who are not present and add to the local_store the ones present.
            match store_handle.get_all(ProviderID::MbedCrypto) {
                Ok(key_triples) => {
                    for key_triple in key_triples.iter().cloned() {
                        let key_id = match key_management::get_key_id(key_triple, &*store_handle) {
                            Ok(key_id) => key_id,
                            Err(response_status) => {
                                error!("Error getting the Key ID for triple:\n{}\n(error: {}), continuing...", key_triple, response_status);
                                to_remove.push(key_triple.clone());
                                continue;
                            }
                        };

                        // Safety: safe because:
                        // * the Mbed Crypto library has been initialized
                        // * this code is executed only by the main thread
                        match unsafe { KeyHandle::open(key_id) } {
                            Ok(_) => {
                                let _ = local_ids_handle.insert(key_id);
                            }
                            Err(ResponseStatus::PsaErrorDoesNotExist) => {
                                to_remove.push(key_triple.clone())
                            }
                            Err(e) => {
                                error!("Error {} when opening a persistent Mbed Crypto key.", e);
                                return None;
                            }
                        };
                    }
                }
                Err(string) => {
                    error!("Key Info Manager error: {}", string);
                    return None;
                }
            };
            for key_triple in to_remove.iter() {
                if let Err(string) = store_handle.remove(key_triple) {
                    error!("Key Info Manager error: {}", string);
                    return None;
                }
            }
        }

        Some(mbed_provider)
    }
}

impl Provide for MbedProvider {
    fn describe(&self) -> Result<(ProviderInfo, HashSet<Opcode>)> {
        Ok((ProviderInfo {
            // Assigned UUID for this provider: 1c1139dc-ad7c-47dc-ad6b-db6fdb466552
            uuid: Uuid::parse_str("1c1139dc-ad7c-47dc-ad6b-db6fdb466552").or(Err(ResponseStatus::InvalidEncoding))?,
            description: String::from("User space software provider, based on Mbed Crypto - the reference implementation of the PSA crypto API"),
            vendor: String::from("Arm"),
            version_maj: 0,
            version_min: 1,
            version_rev: 0,
            id: ProviderID::MbedCrypto,
        }, SUPPORTED_OPCODES.iter().copied().collect()))
    }

    fn psa_generate_key(
        &self,
        app_name: ApplicationName,
        op: psa_generate_key::Operation,
    ) -> Result<psa_generate_key::Result> {
        self.psa_generate_key_internal(app_name, op)
    }

    fn psa_import_key(
        &self,
        app_name: ApplicationName,
        op: psa_import_key::Operation,
    ) -> Result<psa_import_key::Result> {
        self.psa_import_key_internal(app_name, op)
    }

    fn psa_export_public_key(
        &self,
        app_name: ApplicationName,
        op: psa_export_public_key::Operation,
    ) -> Result<psa_export_public_key::Result> {
        self.psa_export_public_key_internal(app_name, op)
    }

    fn psa_destroy_key(
        &self,
        app_name: ApplicationName,
        op: psa_destroy_key::Operation,
    ) -> Result<psa_destroy_key::Result> {
        self.psa_destroy_key_internal(app_name, op)
    }

    fn psa_sign_hash(
        &self,
        app_name: ApplicationName,
        op: psa_sign_hash::Operation,
    ) -> Result<psa_sign_hash::Result> {
        self.psa_sign_hash_internal(app_name, op)
    }

    fn psa_verify_hash(
        &self,
        app_name: ApplicationName,
        op: psa_verify_hash::Operation,
    ) -> Result<psa_verify_hash::Result> {
        self.psa_verify_hash_internal(app_name, op)
    }
}

impl Drop for MbedProvider {
    fn drop(&mut self) {
        // Safety: the Provider was initialized with psa_crypto_init
        unsafe {
            psa_crypto_binding::mbedtls_psa_crypto_free();
        }
    }
}

#[derive(Default, Derivative)]
#[derivative(Debug)]
pub struct MbedProviderBuilder {
    #[derivative(Debug = "ignore")]
    key_info_store: Option<Arc<RwLock<dyn ManageKeyInfo + Send + Sync>>>,
}

impl MbedProviderBuilder {
    pub fn new() -> MbedProviderBuilder {
        MbedProviderBuilder {
            key_info_store: None,
        }
    }

    pub fn with_key_info_store(
        mut self,
        key_info_store: Arc<RwLock<dyn ManageKeyInfo + Send + Sync>>,
    ) -> MbedProviderBuilder {
        self.key_info_store = Some(key_info_store);

        self
    }

    pub fn build(self) -> std::io::Result<MbedProvider> {
        MbedProvider::new(
            self.key_info_store
                .ok_or_else(|| Error::new(ErrorKind::InvalidData, "missing key info store"))?,
        )
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "Mbed Provider initialization failed",
            )
        })
    }
}
