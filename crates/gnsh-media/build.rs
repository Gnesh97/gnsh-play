use std::env;
use std::path::PathBuf;

fn main() {
    let mingw_root = env::var("MINGW_PREFIX").unwrap_or_else(|_| "C:/msys64/mingw64".to_string());
    let mingw_inc = PathBuf::from(&mingw_root).join("include");
    let mingw_lib = PathBuf::from(&mingw_root).join("lib");

    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rustc-link-search=native={}", mingw_lib.display());
    println!("cargo:rustc-link-lib=avcodec");
    println!("cargo:rustc-link-lib=avutil");
    println!("cargo:rustc-link-lib=SDL3");
    println!("cargo:rustc-link-lib=placebo");
    println!("cargo:rustc-link-lib=vulkan-1");

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", mingw_inc.display()))
        .clang_arg("-D_WIN32")
        .allowlist_function("av_.*|avcodec_.*|SDL_InitSubSystem|SDL_OpenAudioDeviceStream|SDL_PutAudioStreamData|SDL_GetAudioStreamQueued|SDL_GetAudioStreamAvailable|SDL_GetAudioStreamData|SDL_ClearAudioStream|SDL_PauseAudioStreamDevice|SDL_ResumeAudioStreamDevice|SDL_SetAudioStreamGain|SDL_DestroyAudioStream|SDL_GetAudioPlaybackDevices|SDL_GetAudioRecordingDevices|SDL_GetAudioDeviceName|SDL_GetAudioDeviceFormat|SDL_free|SDL_GetError|pl_.*")
        .allowlist_type("AV.*|SDL_AudioSpec|SDL_AudioFormat|SDL_AudioStream|SDL_AudioDeviceID|pl_.*|PL_.*|Vk.*")
        .allowlist_var("AV.*|GNSH_.*|SDL_INIT_AUDIO|SDL_AUDIO_.*|PL_.*|pl_.*|VK_.*")
        .derive_default(true)
        .layout_tests(false)
        .generate_comments(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Failed to generate FFmpeg/SDL3 media bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Failed to write media bindings");
}
