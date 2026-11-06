use crate::{Error, Result};

/// Native platform effect, injectable in isolated tests. Production uses Windows Credential
/// Manager. No plaintext fallback exists when the native credential store is unavailable.
pub trait CredentialStore: Send {
    fn get(&self, namespace: &str, key: &str) -> Result<Option<String>>;
    fn set(&self, namespace: &str, key: &str, secret: &str) -> Result<()>;
    fn remove(&self, namespace: &str, key: &str) -> Result<()>;
}

#[derive(Default)]
pub struct WindowsCredentials;

#[cfg(windows)]
fn entry(namespace: &str, key: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(&format!("Komaphone/{namespace}"), key).map_err(|_| Error::Credentials("Cannot open credential entry".into()))
}

#[cfg(windows)]
impl CredentialStore for WindowsCredentials {
    fn get(&self, namespace: &str, key: &str) -> Result<Option<String>> {
        match entry(namespace, key)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(Error::Credentials("Cannot read credential entry".into())),
        }
    }
    fn set(&self, namespace: &str, key: &str, secret: &str) -> Result<()> {
        entry(namespace, key)?.set_password(secret).map_err(|_| Error::Credentials("Cannot save credential entry".into()))?;
        // Confirm persistence before a request can use the credential.
        if self.get(namespace, key)?.as_deref() != Some(secret) {
            return Err(Error::Credentials("Credential readback did not match; pairing was not sent".into()));
        }
        Ok(())
    }
    fn remove(&self, namespace: &str, key: &str) -> Result<()> {
        match entry(namespace, key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(Error::Credentials("Cannot remove credential entry".into())),
        }
    }
}

#[cfg(not(windows))]
impl CredentialStore for WindowsCredentials {
    fn get(&self, _namespace: &str, _key: &str) -> Result<Option<String>> {
        Err(Error::Credentials("Room pairing requires Windows Credential Manager".into()))
    }
    fn set(&self, _namespace: &str, _key: &str, _secret: &str) -> Result<()> {
        Err(Error::Credentials("Room pairing requires Windows Credential Manager".into()))
    }
    fn remove(&self, _namespace: &str, _key: &str) -> Result<()> {
        Err(Error::Credentials("Room pairing requires Windows Credential Manager".into()))
    }
}
