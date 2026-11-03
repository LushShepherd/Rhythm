fn main() {
    println!("cargo:rerun-if-env-changed=RHYTHM_GOOGLE_OAUTH_CLIENT_FILE");
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"));
    let registration = if let Some(path) = std::env::var_os("RHYTHM_GOOGLE_OAUTH_CLIENT_FILE") {
        let path = std::path::PathBuf::from(path);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = std::fs::read(path).expect("Read Rhythm Google desktop OAuth registration");
        assert!(
            bytes.len() <= 64 * 1024,
            "Google OAuth registration file is too large"
        );
        bytes
    } else {
        Vec::new()
    };
    // Desktop OAuth registration is public application metadata. User tokens and
    // recovery keys are never embedded or supplied to the build.
    std::fs::write(output.join("google-oauth-client.json"), registration)
        .expect("Write bundled OAuth registration");
    #[cfg(target_os = "windows")]
    tauri_build::build();
}
