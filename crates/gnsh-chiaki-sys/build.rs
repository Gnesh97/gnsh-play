use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf();

    let chiaki_inc = repo_root.join("third_party/chiaki-ng/lib/include");
    let chiaki_build = repo_root.join("third_party/build/chiaki-ng");
    let chiaki_gen_inc = chiaki_build.join("lib/include");
    let mingw_root = env::var("MINGW_PREFIX").unwrap_or_else(|_| "C:/msys64/mingw64".to_string());
    let mingw_inc = PathBuf::from(&mingw_root).join("include");
    let mingw_lib = PathBuf::from(&mingw_root).join("lib");

    println!("cargo:rerun-if-changed=wrapper.h");
    println!(
        "cargo:rerun-if-changed={}",
        chiaki_build.join("lib/libchiaki.a").display()
    );

    println!(
        "cargo:rustc-link-search=native={}",
        chiaki_build.join("lib").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        chiaki_build.join("third-party").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        chiaki_build.join("third-party/nanopb").display()
    );
    println!("cargo:rustc-link-search=native={}", mingw_lib.display());

    // Static libraries built from chiaki-ng first (GNU ld link order matters).
    println!("cargo:rustc-link-lib=static=chiaki");
    println!("cargo:rustc-link-lib=static=protobuf-nanopb");
    println!("cargo:rustc-link-lib=static=jerasure");
    println!("cargo:rustc-link-lib=static=gf_complete");

    // MSYS2 MINGW64 shared libraries.
    println!("cargo:rustc-link-lib=opus");
    println!("cargo:rustc-link-lib=json-c");
    println!("cargo:rustc-link-lib=miniupnpc");
    println!("cargo:rustc-link-lib=event");
    println!("cargo:rustc-link-lib=curl");
    println!("cargo:rustc-link-lib=crypto");

    // Windows system libraries required by libchiaki.
    println!("cargo:rustc-link-lib=wsock32");
    println!("cargo:rustc-link-lib=ws2_32");
    println!("cargo:rustc-link-lib=bcrypt");
    println!("cargo:rustc-link-lib=iphlpapi");
    println!("cargo:rustc-link-lib=crypt32");

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", chiaki_inc.display()))
        .clang_arg(format!("-I{}", chiaki_gen_inc.display()))
        .clang_arg(format!("-I{}", mingw_inc.display()))
        .clang_arg("-D_WIN32")
        .allowlist_function("chiaki_.*")
        .allowlist_type("Chiaki.*")
        .allowlist_var("CHIAKI_.*")
        .allowlist_type("chiaki_.*")
        .derive_default(true)
        .derive_debug(true)
        .layout_tests(false)
        .generate_comments(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Failed to generate libchiaki bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Failed to write libchiaki bindings");
}
