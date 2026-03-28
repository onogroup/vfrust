use std::path::{Path, PathBuf};

/// Generates a cloud-init ISO image from the given file paths.
///
/// The file paths are categorized by basename: `meta-data`, `user-data`, and
/// optionally `network-config`. At least one of `meta-data` or `user-data` must
/// be present.
///
/// On macOS, this uses `hdiutil makehybrid` to create an ISO 9660 image with
/// volume label "cidata", which is the standard cloud-init NoCloud data source.
pub fn generate_cloud_init_iso(file_paths: &[&str]) -> Result<PathBuf, String> {
    if file_paths.is_empty() {
        return Err("cloud-init requires at least one file path".into());
    }

    // Categorize files by basename
    let mut has_user_data = false;
    let mut has_meta_data = false;
    let mut files: Vec<(&str, &str)> = Vec::new(); // (basename, full_path)

    for path_str in file_paths {
        let path = Path::new(path_str);
        let basename = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("invalid file path: {path_str}"))?;

        match basename {
            "user-data" => has_user_data = true,
            "meta-data" => has_meta_data = true,
            "network-config" => {}
            other => {
                return Err(format!(
                    "unexpected cloud-init file '{other}'; expected one of: \
                     meta-data, user-data, network-config"
                ));
            }
        }

        // Verify the file exists
        if !path.exists() {
            return Err(format!("cloud-init file does not exist: {path_str}"));
        }

        files.push((basename, path_str));
    }

    if !has_user_data && !has_meta_data {
        return Err(
            "cloud-init requires at least one of 'meta-data' or 'user-data'".into(),
        );
    }

    // Create a temporary directory with the cloud-init files
    let tmp_dir = std::env::temp_dir().join(format!("vfrust-cidata-{}", std::process::id()));
    if tmp_dir.exists() {
        std::fs::remove_dir_all(&tmp_dir)
            .map_err(|e| format!("failed to clean up temp dir: {e}"))?;
    }
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("failed to create temp dir: {e}"))?;

    for (basename, source_path) in &files {
        let dest = tmp_dir.join(basename);
        std::fs::copy(source_path, &dest)
            .map_err(|e| format!("failed to copy {source_path} to {}: {e}", dest.display()))?;
    }

    // If meta-data wasn't provided, create an empty one (cloud-init expects it)
    if !has_meta_data {
        let meta_path = tmp_dir.join("meta-data");
        std::fs::write(&meta_path, "")
            .map_err(|e| format!("failed to create empty meta-data: {e}"))?;
    }

    // If user-data wasn't provided, create an empty one
    if !has_user_data {
        let user_path = tmp_dir.join("user-data");
        std::fs::write(&user_path, "")
            .map_err(|e| format!("failed to create empty user-data: {e}"))?;
    }

    // Output ISO path
    let iso_path = std::env::temp_dir().join(format!("vfrust-cloud-init-{}.iso", std::process::id()));

    // Use hdiutil to create the ISO
    let output = std::process::Command::new("hdiutil")
        .args([
            "makehybrid",
            "-iso",
            "-joliet",
            "-default-volume-name",
            "cidata",
            "-o",
        ])
        .arg(&iso_path)
        .arg(&tmp_dir)
        .output()
        .map_err(|e| format!("failed to run hdiutil: {e}"))?;

    // Clean up temp directory
    let _ = std::fs::remove_dir_all(&tmp_dir);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("hdiutil failed: {stderr}"));
    }

    Ok(iso_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_empty_paths_rejected() {
        let result = generate_cloud_init_iso(&[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("at least one file path"));
    }

    #[test]
    fn test_unknown_basename_rejected() {
        let dir = std::env::temp_dir().join("vfrust-ci-test-unknown");
        let _ = fs::create_dir_all(&dir);
        let bad_file = dir.join("random-file");
        fs::write(&bad_file, "test").unwrap();

        let path_str = bad_file.to_str().unwrap().to_string();
        let result = generate_cloud_init_iso(&[&path_str]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unexpected cloud-init file"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_missing_file_rejected() {
        let result = generate_cloud_init_iso(&["/nonexistent/path/user-data"]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn test_no_userdata_or_metadata_rejected() {
        let dir = std::env::temp_dir().join("vfrust-ci-test-nodata");
        let _ = fs::create_dir_all(&dir);
        let nc = dir.join("network-config");
        fs::write(&nc, "test").unwrap();

        let path_str = nc.to_str().unwrap().to_string();
        let result = generate_cloud_init_iso(&[&path_str]);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("at least one of 'meta-data' or 'user-data'"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_generate_iso_with_userdata() {
        let dir = std::env::temp_dir().join("vfrust-ci-test-gen");
        let _ = fs::create_dir_all(&dir);

        let user_data = dir.join("user-data");
        fs::write(&user_data, "#cloud-config\npackages:\n  - vim\n").unwrap();

        let meta_data = dir.join("meta-data");
        fs::write(&meta_data, "instance-id: test-vm\n").unwrap();

        let ud_str = user_data.to_str().unwrap().to_string();
        let md_str = meta_data.to_str().unwrap().to_string();

        let result = generate_cloud_init_iso(&[&md_str, &ud_str]);
        match result {
            Ok(iso_path) => {
                assert!(iso_path.exists(), "ISO file should have been created");
                // Clean up
                let _ = fs::remove_file(&iso_path);
            }
            Err(e) => {
                // hdiutil may not be available in all test environments
                if !e.contains("hdiutil") {
                    panic!("unexpected error: {e}");
                }
            }
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
