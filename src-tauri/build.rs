fn main() {
    println!("cargo:rerun-if-changed=embedded/hushell_tray.dll");
    // tauri_build embeds icons/icon.ico (+ manifest + version) via the
    // resource compiler found in PATH — llvm-rc from llvm-mingw on this
    // cross box. The icon must stay a real multi-size .ico: a single-layer
    // one reads as "no icon" at large Explorer sizes.
    tauri_build::build()
}
