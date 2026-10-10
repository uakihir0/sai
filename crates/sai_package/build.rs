use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    if let Err(error) = generate_build_id() {
        panic!("could not compute the SAI toolchain build ID: {error}");
    }
}

fn generate_build_id() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .ok_or("Cargo did not provide CARGO_MANIFEST_DIR to the sai_package build script")?,
    );
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .ok_or("sai_package is not below the workspace root")?;
    let workspace_manifest = workspace_root.join("Cargo.toml");
    let lock_file = workspace_root.join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", workspace_manifest.display());
    println!("cargo:rerun-if-changed={}", lock_file.display());
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=TARGET");
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=SAI_BUILD_FEATURES");

    let version = env::var("CARGO_PKG_VERSION")?;
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let rustc_version = Command::new(rustc).arg("-vV").output()?;
    if !rustc_version.status.success() {
        return Err(format!("rustc -vV failed with status {}", rustc_version.status).into());
    }
    let target = env::var("TARGET")?;
    let profile = env::var("PROFILE")?;
    let mut features = match env::var("SAI_BUILD_FEATURES") {
        Ok(features) => features
            .split(',')
            .map(str::trim)
            .filter(|feature| !feature.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        Err(env::VarError::NotPresent) => env::vars()
            .filter_map(|(name, value)| {
                (name.starts_with("CARGO_FEATURE_") && value == "1")
                    .then(|| name["CARGO_FEATURE_".len()..].to_owned())
            })
            .collect::<Vec<_>>(),
        Err(error) => return Err(error.into()),
    };
    features.sort();
    features.dedup();

    let (member_patterns, exclude_patterns) = workspace_member_patterns(&workspace_manifest)?;
    watch_workspace_pattern_directories(workspace_root, &member_patterns)?;
    watch_workspace_pattern_directories(workspace_root, &exclude_patterns)?;
    let excluded_members = expand_workspace_patterns(workspace_root, &exclude_patterns, false)?;
    let members = expand_workspace_patterns(workspace_root, &member_patterns, true)?;

    let mut files = vec![workspace_manifest.clone(), lock_file];
    let mut active_directories = BTreeSet::new();
    for member in members {
        if excluded_members
            .iter()
            .any(|excluded| member == *excluded || member.starts_with(excluded))
        {
            continue;
        }
        if !member.is_dir() {
            return Err(format!("workspace member {member:?} is not a directory").into());
        }
        collect_files(&member, &mut files, &mut active_directories)?;
    }
    let library = workspace_root.join("library");
    println!("cargo:rerun-if-changed={}", library.display());
    if library.is_dir() {
        collect_files(&library, &mut files, &mut active_directories)?;
    }
    files.sort();
    files.dedup();
    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
    }

    let mut hasher = blake3::Hasher::new();
    hash_field(&mut hasher, "toolchain-version", version.as_bytes());
    hash_field(&mut hasher, "rustc-version", &rustc_version.stdout);
    hash_field(&mut hasher, "target", target.as_bytes());
    hash_field(&mut hasher, "profile", profile.as_bytes());
    hash_field(&mut hasher, "features", features.join("\0").as_bytes());
    for file in files {
        let relative_path = file
            .strip_prefix(workspace_root)
            .expect("build-ID input is within the workspace");
        let relative_path = relative_path.to_string_lossy();
        let contents = fs::read(&file)?;
        hash_field(&mut hasher, relative_path.as_ref(), &contents);
    }

    println!(
        "cargo:rustc-env=SAI_BUILD_ID={}",
        &hasher.finalize().to_hex().as_str()[..16]
    );
    Ok(())
}

fn workspace_member_patterns(
    manifest: &Path,
) -> Result<(Vec<String>, Vec<String>), Box<dyn std::error::Error>> {
    let contents = fs::read_to_string(manifest)?;
    let value = toml::from_str::<toml::Value>(&contents)?;
    let workspace = value
        .get("workspace")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "workspace is not a table"))?;
    let members = string_array(workspace, "members", true)?;
    let excludes = string_array(workspace, "exclude", false)?;
    Ok((members, excludes))
}

fn string_array(
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
    required: bool,
) -> Result<Vec<String>, io::Error> {
    let Some(value) = table.get(key) else {
        return if required {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("workspace.{key} is missing"),
            ))
        } else {
            Ok(Vec::new())
        };
    };
    let values = value.as_array().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("workspace.{key} is not an array"),
        )
    })?;
    let mut members = Vec::new();
    for value in values {
        let member = value.as_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("workspace.{key} must contain only strings"),
            )
        })?;
        members.push(member.to_owned());
    }
    Ok(members)
}

fn expand_workspace_patterns(
    workspace_root: &Path,
    patterns: &[String],
    require_matches: bool,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let escaped_root = glob::Pattern::escape(
        workspace_root
            .to_str()
            .ok_or("workspace root path is not valid UTF-8")?,
    );
    let mut paths = Vec::new();
    for member_pattern in patterns {
        let pattern = format!("{escaped_root}/{member_pattern}");
        let matches = glob::glob(&pattern)?.collect::<Result<Vec<_>, _>>()?;
        if require_matches && matches.is_empty() {
            return Err(format!("workspace member pattern {pattern:?} matched no paths").into());
        }
        paths.extend(matches);
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn watch_workspace_pattern_directories(
    workspace_root: &Path,
    patterns: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let escaped_root = glob::Pattern::escape(
        workspace_root
            .to_str()
            .ok_or("workspace root path is not valid UTF-8")?,
    );
    let mut watched = BTreeSet::new();
    for pattern in patterns {
        let components = pattern.split('/').collect::<Vec<_>>();
        for (index, component) in components.iter().enumerate() {
            if !component
                .chars()
                .any(|character| matches!(character, '*' | '?' | '[' | ']' | '{' | '}'))
            {
                continue;
            }
            for prefix in [
                components[..index].join("/"),
                components[..=index].join("/"),
            ] {
                let prefix = if prefix.is_empty() {
                    escaped_root.clone()
                } else {
                    format!("{escaped_root}/{prefix}")
                };
                for path in glob::glob(&prefix)?.collect::<Result<Vec<_>, _>>()? {
                    if path.is_dir() && watched.insert(path.clone()) {
                        println!("cargo:rerun-if-changed={}", path.display());
                    }
                }
            }
        }
    }
    Ok(())
}

fn collect_files(
    directory: &Path,
    files: &mut Vec<PathBuf>,
    active_directories: &mut BTreeSet<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let canonical_directory = fs::canonicalize(directory)?;
    if !active_directories.insert(canonical_directory.clone()) {
        return Err(format!("workspace source directory cycle at {directory:?}").into());
    }
    println!("cargo:rerun-if-changed={}", directory.display());
    if canonical_directory != directory {
        println!("cargo:rerun-if-changed={}", canonical_directory.display());
    }
    let mut entries = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();
    for path in entries {
        let metadata = fs::metadata(&path)?;
        if metadata.is_dir() {
            collect_files(&path, files, active_directories)?;
        } else if metadata.is_file() {
            let canonical_path = fs::canonicalize(&path)?;
            if canonical_path != path {
                println!("cargo:rerun-if-changed={}", canonical_path.display());
            }
            files.push(path);
        }
    }
    active_directories.remove(&canonical_directory);
    Ok(())
}

fn hash_field(hasher: &mut blake3::Hasher, name: &str, contents: &[u8]) {
    hasher.update(&(name.len() as u64).to_le_bytes());
    hasher.update(name.as_bytes());
    hasher.update(&(contents.len() as u64).to_le_bytes());
    hasher.update(contents);
}
