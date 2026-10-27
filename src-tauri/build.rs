fn main() {
    tauri_build::build();
    // MinGW 16 injects its own manifest, which conflicts with Tauri's resource manifest.
    if std::env::var("TARGET").is_ok_and(|target| target.ends_with("windows-gnu")) {
        let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
        let specs = out.join("jarvis.specs");
        std::fs::write(&specs, "*endfile:\ncrtend.o%s\n").expect("write linker specs");
        println!("cargo:rustc-link-arg=-specs={}", specs.display());
    }
}
