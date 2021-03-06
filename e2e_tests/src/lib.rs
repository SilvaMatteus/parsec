// Copyright 2020 Contributors to the Parsec project.
// SPDX-License-Identifier: Apache-2.0
pub mod raw_request;
pub mod stress;

pub use raw_request::RawRequestClient;

pub use parsec_client::core::request_client::RequestClient;
pub use parsec_client::error;

use log::error;
use parsec_client::auth::AuthenticationData;
use parsec_client::core::basic_client::BasicClient;
use parsec_client::core::interface::operations::list_providers::ProviderInfo;
use parsec_client::core::interface::operations::psa_algorithm::{
    Algorithm, AsymmetricSignature, Hash,
};
use parsec_client::core::interface::operations::psa_key_attributes::{
    Attributes, Lifetime, Policy, Type, UsageFlags,
};
use parsec_client::core::interface::requests::{Opcode, ProviderID, ResponseStatus, Result};
use parsec_client::error::Error;
use std::collections::HashSet;
use std::time::Duration;

/// Client structure automatically choosing a provider and high-level operation functions.
#[derive(Debug)]
pub struct TestClient {
    basic_client: BasicClient,
    created_keys: Option<HashSet<(String, String, ProviderID)>>,
}

fn convert_error(err: Error) -> ResponseStatus {
    if let Error::Service(resp_status) = err {
        resp_status
    } else {
        panic!(
            "Expected to obtain a service error, but got a client error instead: {:?}",
            err
        );
    }
}

impl TestClient {
    /// Creates a TestClient instance.
    ///
    /// The implicit provider chosen for servicing cryptographic operations is decided through
    /// a call to `list_providers`, followed by choosing the first non-Core provider.
    pub fn new() -> TestClient {
        let mut client = TestClient {
            basic_client: BasicClient::new(AuthenticationData::AppIdentity(String::from("root"))),
            created_keys: Some(HashSet::new()),
        };

        let crypto_provider = client.find_crypto_provider();
        client.set_provider(crypto_provider);
        client
            .basic_client
            .set_timeout(Some(Duration::from_secs(10)));

        client
    }

    fn find_crypto_provider(&self) -> ProviderID {
        let providers = self
            .basic_client
            .list_providers()
            .expect("List providers failed");
        for provider in providers {
            if provider.id != ProviderID::Core {
                return provider.id;
            }
        }

        ProviderID::Core
    }

    /// Manually set the provider to execute the requests.
    pub fn set_provider(&mut self, provider: ProviderID) {
        self.basic_client.set_implicit_provider(provider);
    }

    /// Get client provider
    pub fn provider(&self) -> Option<ProviderID> {
        self.basic_client.implicit_provider()
    }

    /// Set the client authentication string.
    pub fn set_auth(&mut self, auth: String) {
        self.basic_client
            .set_auth_data(AuthenticationData::AppIdentity(auth));
    }

    /// Get client authentication string.
    pub fn auth(&self) -> String {
        if let AuthenticationData::AppIdentity(app_name) = self.basic_client.auth_data() {
            app_name
        } else {
            panic!("Client should always be using AppIdentity-based authentication");
        }
    }

    /// By default the `TestClient` instance will destroy the keys it created when it is dropped,
    /// unless this function is called.
    pub fn do_not_destroy_keys(&mut self) {
        let _ = self.created_keys.take();
    }

    /// Creates a key with specific attributes.
    pub fn generate_key(&mut self, key_name: String, attributes: Attributes) -> Result<()> {
        self.basic_client
            .psa_generate_key(key_name.clone(), attributes)
            .map_err(convert_error)?;

        let provider = self.provider().unwrap();
        let auth = self.auth();

        if let Some(ref mut created_keys) = self.created_keys {
            let _ = created_keys.insert((key_name, auth, provider));
        }

        Ok(())
    }

    /// Generate a 1024 bits RSA key pair.
    /// The key can only be used for signing/verifying with the RSA PKCS 1v15 signing algorithm with SHA-256 and exporting its public part.
    pub fn generate_rsa_sign_key(&mut self, key_name: String) -> Result<()> {
        self.generate_key(
            key_name,
            Attributes {
                lifetime: Lifetime::Persistent,
                key_type: Type::RsaKeyPair,
                bits: 1024,
                policy: Policy {
                    usage_flags: UsageFlags {
                        sign_hash: true,
                        verify_hash: true,
                        sign_message: true,
                        verify_message: true,
                        export: true,
                        encrypt: false,
                        decrypt: false,
                        cache: false,
                        copy: false,
                        derive: false,
                    },
                    permitted_algorithms: Algorithm::AsymmetricSignature(
                        AsymmetricSignature::RsaPkcs1v15Sign {
                            hash_alg: Hash::Sha256.into(),
                        },
                    ),
                },
            },
        )
    }

    /// Imports and creates a key with specific attributes.
    pub fn import_key(
        &mut self,
        key_name: String,
        attributes: Attributes,
        data: Vec<u8>,
    ) -> Result<()> {
        self.basic_client
            .psa_import_key(key_name.clone(), data, attributes)
            .map_err(convert_error)?;

        let provider = self.provider().unwrap();
        let auth = self.auth();

        if let Some(ref mut created_keys) = self.created_keys {
            let _ = created_keys.insert((key_name, auth, provider));
        }

        Ok(())
    }

    /// Import a 1024 bits RSA public key.
    /// The key can only be used for verifying with the RSA PKCS 1v15 signing algorithm with SHA-256.
    pub fn import_rsa_public_key(&mut self, key_name: String, data: Vec<u8>) -> Result<()> {
        self.import_key(
            key_name,
            Attributes {
                lifetime: Lifetime::Persistent,
                key_type: Type::RsaPublicKey,
                bits: 1024,
                policy: Policy {
                    usage_flags: UsageFlags {
                        sign_hash: false,
                        verify_hash: true,
                        sign_message: false,
                        verify_message: true,
                        export: false,
                        encrypt: false,
                        decrypt: false,
                        cache: false,
                        copy: false,
                        derive: false,
                    },
                    permitted_algorithms: Algorithm::AsymmetricSignature(
                        AsymmetricSignature::RsaPkcs1v15Sign {
                            hash_alg: Hash::Sha256.into(),
                        },
                    ),
                },
            },
            data,
        )
    }

    /// Exports a public key.
    pub fn export_public_key(&mut self, key_name: String) -> Result<Vec<u8>> {
        self.basic_client
            .psa_export_public_key(key_name)
            .map_err(convert_error)
    }

    /// Destroys a key.
    pub fn destroy_key(&mut self, key_name: String) -> Result<()> {
        self.basic_client
            .psa_destroy_key(key_name.clone())
            .map_err(convert_error)?;

        let provider = self.provider().unwrap();
        let auth = self.auth();

        if let Some(ref mut created_keys) = self.created_keys {
            let _ = created_keys.remove(&(key_name, auth, provider));
        }

        Ok(())
    }

    /// Signs a short digest with a key.
    pub fn sign(
        &mut self,
        key_name: String,
        alg: AsymmetricSignature,
        hash: Vec<u8>,
    ) -> Result<Vec<u8>> {
        self.basic_client
            .psa_sign_hash(key_name, hash, alg)
            .map_err(convert_error)
    }

    /// Signs a short digest with an RSA key.
    pub fn sign_with_rsa_sha256(&mut self, key_name: String, hash: Vec<u8>) -> Result<Vec<u8>> {
        self.sign(
            key_name,
            AsymmetricSignature::RsaPkcs1v15Sign {
                hash_alg: Hash::Sha256.into(),
            },
            hash,
        )
    }

    /// Verifies a signature.
    pub fn verify(
        &mut self,
        key_name: String,
        alg: AsymmetricSignature,
        hash: Vec<u8>,
        signature: Vec<u8>,
    ) -> Result<()> {
        self.basic_client
            .psa_verify_hash(key_name, hash, alg, signature)
            .map_err(convert_error)
    }

    /// Verifies a signature made with an RSA key.
    pub fn verify_with_rsa_sha256(
        &mut self,
        key_name: String,
        hash: Vec<u8>,
        signature: Vec<u8>,
    ) -> Result<()> {
        self.verify(
            key_name,
            AsymmetricSignature::RsaPkcs1v15Sign {
                hash_alg: Hash::Sha256.into(),
            },
            hash,
            signature,
        )
    }

    /// Lists the provider available for the Parsec service.
    pub fn list_providers(&mut self) -> Result<Vec<ProviderInfo>> {
        self.basic_client.list_providers().map_err(convert_error)
    }

    /// Lists the opcodes available for one provider to execute.
    pub fn list_opcodes(&mut self, provider_id: ProviderID) -> Result<HashSet<Opcode>> {
        self.basic_client
            .list_opcodes(provider_id)
            .map_err(convert_error)
    }

    /// Executes a ping operation.
    pub fn ping(&mut self) -> Result<(u8, u8)> {
        self.basic_client.ping().map_err(convert_error)
    }
}

impl Default for TestClient {
    fn default() -> Self {
        TestClient::new()
    }
}

impl Drop for TestClient {
    fn drop(&mut self) {
        if let Some(ref mut created_keys) = self.created_keys {
            for (key_name, auth, provider) in created_keys.clone().iter() {
                self.set_provider(*provider);
                self.set_auth(auth.clone());
                if self.destroy_key(key_name.clone()).is_err() {
                    error!("Failed to destroy key '{}'", key_name);
                }
            }
        }
    }
}
