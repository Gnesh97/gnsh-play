use std::env;
use std::path::PathBuf;

fn main() {
    let mingw_root = env::var("MINGW_PREFIX").unwrap_or_else(|_| "C:/msys64/mingw64".to_string());
    let mingw_inc = PathBuf::from(&mingw_root).join("include");
    let mingw_lib = PathBuf::from(&mingw_root).join("lib");

    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rustc-link-search=native={}", mingw_lib.display());
    println!("cargo:rustc-link-lib=SDL3");

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", mingw_inc.display()))
        .clang_arg("-D_WIN32")
        .allowlist_function("SDL_.*")
        .allowlist_type("SDL_.*")
        .allowlist_var("SDL_.*|SDLK_.*|GNSH_SDL_.*")
        .derive_default(true)
        .layout_tests(false)
        .generate_comments(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Failed to generate SDL3 bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Failed to write SDL3 bindings");
}
