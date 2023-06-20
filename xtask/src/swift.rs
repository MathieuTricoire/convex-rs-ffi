use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use clap::Subcommand;
use uniffi_bindgen::bindings::TargetLanguage;
use xshell::{cmd, Shell};

use crate::{utils, workspace};

#[derive(Subcommand)]
pub enum SwiftCommand {
    /// Builds the Swift XCFramework.
    #[command(name = "build-xcframework")]
    BuildXCFramework {
        /// Build with the release profile
        #[clap(long)]
        release: bool,
    },
}

impl SwiftCommand {
    pub fn run(self) -> Result<()> {
        let sh = Shell::new()?;
        let _d = sh.push_dir(workspace::metadata()?.root_dir);
        match self {
            SwiftCommand::BuildXCFramework { release } => {
                let profile = if release { "release" } else { "dev" };
                build_xcframework(&sh, profile)
            }
        }
    }
}

enum Library {
    Single {
        target: &'static str,
    },
    Multiple {
        targets: Vec<&'static str>,
        lipo: &'static str,
    },
}

fn build_xcframework(sh: &Shell, profile: &str) -> Result<()> {
    let cargo = utils::cargo_path();
    let workspace::Metadata {
        root_dir,
        target_dir,
    } = workspace::metadata()?;

    let generated_dir = root_dir.join("generated");
    let templates_dir = root_dir.join("templates");

    let swift_dir = generated_dir.join("swift");
    if fs::metadata(&swift_dir).is_ok() {
        fs::remove_dir_all(swift_dir.as_path())?;
    }

    let tmp_dir = swift_dir.join("tmp");

    let uniffi_dir = tmp_dir.join("uniffi");
    let libs_dir = tmp_dir.join("libs");
    let headers_dir = tmp_dir.join("headers");
    let src_dir = swift_dir.join("Sources/ConvexFFI");
    fs::create_dir_all(uniffi_dir.clone())?;
    fs::create_dir_all(libs_dir.clone())?;
    fs::create_dir_all(headers_dir.clone())?;
    fs::create_dir_all(src_dir.clone())?;

    let profile_dir_name = if profile == "dev" { "debug" } else { profile };

    let libraries = [
        // iOS (Apple Silicon)
        Library::Single {
            target: "aarch64-apple-ios",
        },
        // iOS simulator (Apple Silicon, Intel x86)
        Library::Multiple {
            targets: vec!["aarch64-apple-ios-sim", "x86_64-apple-ios"],
            lipo: "libconvex_ffi_ios_sim.a",
        },
        // macOS (Apple Silicon, Intel x86)
        Library::Multiple {
            targets: vec!["aarch64-apple-darwin", "x86_64-apple-darwin"],
            lipo: "libconvex_ffi_macos.a",
        },
    ];

    println!("Building libraries for Swift.");
    let mut cmd = cmd!(sh, "{cargo} build -p convex-ffi --profile {profile}");
    // Remove debug info in release mode like Mozilla
    // see: https://github.com/mozilla/application-services/blob/77e45817376b43586205bd1f58ea847a5472eda0/megazords/ios-rust/build-xcframework.sh#L67-L69
    if profile == "release" {
        cmd = cmd.env("RUSTFLAGS", "-C debuginfo=0");
    }
    for library in &libraries {
        match library {
            Library::Single { target } => cmd = cmd.arg("--target").arg(target),
            Library::Multiple { targets, .. } => {
                for target in targets {
                    cmd = cmd.arg("--target").arg(target);
                }
            }
        }
    }
    cmd.run()?;

    let mut xcframework_libs = vec![];
    for library in libraries {
        match library {
            Library::Single { target } => xcframework_libs.push(
                target_dir
                    .join(target)
                    .join(profile_dir_name)
                    .join("libconvex_ffi.a"),
            ),
            Library::Multiple { lipo, targets } => {
                println!("Creating fat library: {:?} => {}", targets, lipo);

                let input_libs = targets.into_iter().map(|target| {
                    target_dir
                        .join(target)
                        .join(profile_dir_name)
                        .join("libconvex_ffi.a")
                });
                let output_lib = libs_dir.join(lipo);

                cmd!(sh, "lipo -create {input_libs...} -output {output_lib}").run()?;
                xcframework_libs.push(output_lib);
            }
        }
    }

    println!("Generating uniffi files");
    let udl_file = Utf8PathBuf::from_path_buf(root_dir.join("convex-ffi/src/lib.udl")).unwrap();
    let out_dir = Utf8Path::from_path(&uniffi_dir).unwrap();
    // Necessary to extract uniffi interface definition from code, see: https://mozilla.github.io/uniffi-rs/proc_macro/index.html
    let lib_file = xcframework_libs
        .first()
        .map(|file| Utf8Path::from_path(file).unwrap());
    uniffi_bindgen::generate_bindings(
        udl_file.as_path(),
        None,
        vec![TargetLanguage::Swift],
        Some(out_dir),
        lib_file,
        false,
    )?;

    fs::rename(
        uniffi_dir.join("convex_ffiFFI.h"),
        headers_dir.join("convex_ffiFFI.h"),
    )?;
    fs::rename(
        uniffi_dir.join("convex_ffiFFI.modulemap"),
        headers_dir.join("module.modulemap"),
    )?;
    fs::rename(
        uniffi_dir.join("convex_ffi.swift"),
        src_dir.join("ConvexFFI.swift"),
    )?;
    fs::copy(
        templates_dir.join("PackageTemplate.swift"),
        swift_dir.join("Package.swift"),
    )?;

    println!("Generating ConvexFFI XCFramework");
    let mut cmd = cmd!(sh, "xcodebuild -create-xcframework");
    for lib in xcframework_libs {
        cmd = cmd
            .arg("-library")
            .arg(lib)
            .arg("-headers")
            .arg(headers_dir.as_path())
    }
    cmd.arg("-output")
        .arg(swift_dir.join("ConvexFFI.xcframework").as_path())
        .run()?;

    // In case of release create a zip and compute checksum?

    println!("Update ConvexFFI.swift file");
    fs::remove_dir_all(tmp_dir.as_path())?;

    update_swift_file(src_dir.join("ConvexFFI.swift"))?;

    Ok(())
}

fn update_swift_file(file_path: PathBuf) -> Result<()> {
    let mut file = File::open(&file_path)?;

    let mut contents = String::new();
    file.read_to_string(&mut contents)?;

    let new_contents = contents.replace(
        "public enum Value {",
        "@dynamicMemberLookup\npublic enum Value: Sendable {",
    );

    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&file_path)?;

    file.write_all(new_contents.as_bytes())?;

    Ok(())
}
