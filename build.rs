#[cfg(windows)]
fn windows_resources() {
    println!("cargo:rerun-if-changed=assets/TermShot.rc");
    println!("cargo:rerun-if-changed=assets/TermShot.ico");
    println!("cargo:rerun-if-changed=assets/TermShot.manifest");
    embed_resource::compile("assets/TermShot.rc", embed_resource::NONE)
        .manifest_required()
        .expect("could not embed the Windows application icon and manifest");
}

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        #[cfg(windows)]
        windows_resources();
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        build_mac();
    }
}

fn build_mac() {
    use std::{env, path::PathBuf, process::Command};
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => panic!("unsupported macOS architecture {other}"),
    };
    println!("cargo:rerun-if-changed=macos/Snapshot.h");
    println!("cargo:rerun-if-changed=macos/CaptureOverlay.h");
    for name in ["TermShot", "Snapshot", "CaptureOverlay"] {
        let source = format!("macos/{name}.m");
        let obj = out.join(format!("{name}.o"));
        println!("cargo:rerun-if-changed={source}");
        let status = Command::new("xcrun")
            .args([
                "--sdk",
                "macosx",
                "clang",
                "-arch",
                arch,
                "-mmacosx-version-min=14.0",
                "-O2",
                "-fobjc-arc",
                "-c",
                &source,
                "-o",
            ])
            .arg(&obj)
            .status()
            .expect("requires Xcode command line tools");
        assert!(status.success(), "macOS frontend compilation failed");
        println!("cargo:rustc-link-arg={}", obj.display());
    }
    // Rust omits the Clang runtime that implements Objective-C availability checks.
    let runtime = Command::new("xcrun")
        .args([
            "--sdk",
            "macosx",
            "clang",
            "-arch",
            arch,
            "--print-libgcc-file-name",
        ])
        .output()
        .expect("could not locate the macOS Clang runtime");
    assert!(runtime.status.success(), "Clang did not report its runtime");
    let runtime = String::from_utf8(runtime.stdout).expect("invalid Clang runtime path");
    let runtime = PathBuf::from(runtime.trim());
    assert!(
        runtime.is_file(),
        "missing Clang runtime at {}",
        runtime.display()
    );
    println!("cargo:rustc-link-arg={}", runtime.display());

    for framework in [
        "QuartzCore",
        "AppKit",
        "ScreenCaptureKit",
        "CoreGraphics",
        "CoreFoundation",
        "CoreMedia",
        "CoreVideo",
        "VideoToolbox",
        "Carbon",
        "Foundation",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    println!("cargo:rustc-link-lib=objc");
}
