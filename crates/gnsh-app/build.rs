use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_windres() -> PathBuf {
    let fallback = Path::new(r"C:\msys64\mingw64\bin\windres.exe");
    if Command::new("windres").arg("--version").output().is_ok() {
        PathBuf::from("windres")
    } else if fallback.exists() {
        fallback.to_path_buf()
    } else {
        panic!("windres not found in PATH or at {}", fallback.display());
    }
}

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let assets_dir = manifest_dir.join("assets");
    let rc_path = assets_dir.join("gnsh-play.rc");
    let ico_path = assets_dir.join("gnsh-play.ico");

    println!("cargo:rerun-if-changed={}", rc_path.display());
    println!("cargo:rerun-if-changed={}", ico_path.display());

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let res_obj = out_dir.join("gnsh-play-res.o");

    let windres = find_windres();
    let status = Command::new(&windres)
        .arg("-I")
        .arg(&assets_dir)
        .arg("-i")
        .arg(&rc_path)
        .arg("-o")
        .arg(&res_obj)
        .arg("-O")
        .arg("coff")
        .status()
        .unwrap_or_else(|err| panic!("failed spawning {}: {err}", windres.display()));

    if !status.success() {
        panic!(
            "windres failed compiling {} with status {status}",
            rc_path.display()
        );
    }

    println!("cargo:rustc-link-arg-bin=gnsh-play={}", res_obj.display());
}
