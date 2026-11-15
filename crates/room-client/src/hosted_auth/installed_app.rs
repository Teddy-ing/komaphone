//! The verified Windows package supplies a path, never a secret in arguments or
//! environment variables. Offline startup and session revocation do not use it.
use super::{Client, MAX_AUTH_BYTES, invalid};
use crate::Result;
use std::{fs::File, io::Read, path::Path};

impl Client {
    /// Load the public Desktop OAuth client configuration bundled by the verified
    /// Komaphone launcher. Only sign-in uses this factory; sign-out uses `new`.
    pub fn from_installed_app(trusted_certificate_pem: Option<&[u8]>) -> Result<Self> {
        let path = std::env::var_os("KOMAPHONE_GOOGLE_CLIENT_CONFIG");
        Self::from_installed_path(trusted_certificate_pem, path.as_deref().map(Path::new))
    }

    fn from_installed_path(trusted_certificate_pem: Option<&[u8]>, path: Option<&Path>) -> Result<Self> {
        let bytes = read_configuration(path)?;
        Self::with_installed_client_json(trusted_certificate_pem, &bytes)
    }
}

fn read_configuration(path: Option<&Path>) -> Result<Vec<u8>> {
    let path = path.filter(|path| path.is_absolute()).ok_or_else(|| {
        invalid("Google sign-in needs the installed Komaphone package configuration. Open Komaphone through its launcher; offline drawing remains available")
    })?;
    let file =
        File::open(path).map_err(|_| invalid("Cannot read Google sign-in configuration. Reinstall the verified Komaphone package or use offline drawing"))?;
    let metadata = file.metadata().map_err(|_| invalid("Cannot inspect the installed Google sign-in configuration"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_AUTH_BYTES as u64 {
        return Err(invalid("Installed Google sign-in configuration must be a regular file within its size limit"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_AUTH_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|_| invalid("Cannot read the installed Google sign-in configuration"))?;
    if bytes.is_empty() || bytes.len() > MAX_AUTH_BYTES {
        return Err(invalid("Installed Google sign-in configuration exceeds its size limit"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn configuration() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"installed":{"client_id":super::super::GOOGLE_CLIENT_ID,"client_secret":"synthetic-installed-client-secret"}}))
            .unwrap()
    }
    #[test]
    fn installed_package_file_loads_without_browser_or_network() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("google-desktop-client.json");
        std::fs::write(&path, configuration()).unwrap();
        assert!(Client::from_installed_path(None, Some(&path)).is_ok());
        assert!(Client::new(None).is_ok(), "service configuration and revocation do not depend on this file");
    }
    #[test]
    fn missing_relative_directory_empty_and_oversized_files_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.json");
        let empty = temp.path().join("empty.json");
        let large = temp.path().join("large.json");
        std::fs::write(&empty, []).unwrap();
        std::fs::write(&large, vec![b'x'; MAX_AUTH_BYTES + 1]).unwrap();
        for path in [None, Some(Path::new("relative.json")), Some(missing.as_path()), Some(temp.path()), Some(empty.as_path()), Some(large.as_path())] {
            assert!(Client::from_installed_path(None, path).is_err());
        }
        assert!(Client::new(None).is_ok());
    }
    #[test]
    fn malformed_and_wrong_client_files_never_reflect_contents() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("configuration.json");
        let marker = "private-fixture-content-must-not-be-reflected";
        for bytes in
            [marker.as_bytes().to_vec(), serde_json::to_vec(&serde_json::json!({"installed":{"client_id":"wrong-client","client_secret":marker}})).unwrap()]
        {
            std::fs::write(&path, bytes).unwrap();
            let Err(error) = Client::from_installed_path(None, Some(&path)) else { panic!("Invalid configuration was accepted") };
            assert!(!error.to_string().contains(marker));
            assert!(Client::new(None).is_ok());
        }
    }
}
