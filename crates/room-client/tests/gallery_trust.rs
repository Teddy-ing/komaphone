#![cfg(not(target_arch = "wasm32"))]
use photocraft_room_client::ClientOptions;

#[test]
fn explicit_private_certificate_file_is_bounded_and_never_disables_verification() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ca.pem");
    assert!(ClientOptions::with_trusted_certificate_file(&path).is_err());
    for bytes in [Vec::new(), vec![b'x'; 65_537], b"not a certificate".to_vec(), b"-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----\n".to_vec()] {
        std::fs::write(&path, bytes).unwrap();
        assert!(ClientOptions::with_trusted_certificate_file(&path).is_err());
    }
    assert!(ClientOptions::with_trusted_certificate_file(root.path()).is_err());
    let certificate = include_bytes!("../../../apps/komaphone-service/tests/fixtures/tls/localhost.pem");
    std::fs::write(&path, certificate).unwrap();
    assert_eq!(ClientOptions::with_trusted_certificate_file(&path).unwrap().trusted_certificate_pem.as_deref(), Some(certificate.as_slice()));
}
