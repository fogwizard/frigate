//! ONNX Runtime's prebuilt CoreML objects call `___isPlatformVersionAtLeast`.
//! Rust links with `-nodefaultlibs`, so Clang's copy of that function is not
//! pulled in. Link only `os_version_check.c.o` from the toolchain runtime.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref() != Some("macos") {
        return;
    }
    let obj = version_check_object().expect("locating Clang os_version_check.c.o");
    println!("cargo:rustc-link-arg={}", obj.display());
}

fn version_check_object() -> Result<PathBuf, String> {
    let resource_dir = clang_resource_dir()?;
    let runtime = resource_dir.join("lib/darwin/libclang_rt.osx.a");
    if !runtime.is_file() {
        return Err(format!("missing {}", runtime.display()));
    }

    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => return Err(format!("unsupported macOS arch {other:?}")),
    };
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?);
    let thin = out_dir.join("libclang_rt.osx.a");
    run(
        "lipo",
        [
            "-thin",
            arch,
            "-output",
            thin.to_str().ok_or("thin archive path")?,
            runtime.to_str().ok_or("clang runtime path")?,
        ],
    )?;

    let obj_dir = out_dir.join("clang_rt");
    std::fs::create_dir_all(&obj_dir).map_err(|e| e.to_string())?;
    let status = Command::new("ar")
        .current_dir(&obj_dir)
        .args(["-x"])
        .arg(&thin)
        .arg("os_version_check.c.o")
        .status()
        .map_err(|e| format!("ar: {e}"))?;
    if !status.success() {
        return Err(format!("ar exited with {status}"));
    }
    let obj = obj_dir.join("os_version_check.c.o");
    if !obj.is_file() {
        return Err(format!("missing {}", obj.display()));
    }
    println!("cargo:rerun-if-changed={}", runtime.display());
    Ok(obj)
}

fn clang_resource_dir() -> Result<PathBuf, String> {
    let output = Command::new("xcrun")
        .args(["--sdk", "macosx", "clang", "-print-resource-dir"])
        .output()
        .map_err(|e| format!("xcrun clang: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "xcrun clang failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let dir = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
    Ok(PathBuf::from(dir.trim()))
}

fn run(cmd: &str, args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>) -> Result<(), String> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cmd} exited with {status}"))
    }
}
