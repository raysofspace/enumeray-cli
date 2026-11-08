//! Suggested download basenames shared by the connector, service and CLI.

pub fn valid_filename(name: &str) -> bool {
    if !crate::collections::valid_path(name) || name.contains(['/', '<', '>', '"', '|', '?', '*']) {
        return false;
    }
    let stem = name.split('.').next().unwrap_or_default().trim_end();
    let stem = stem.to_ascii_uppercase();
    !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(
                    number,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
}

/// Prefer the measured wrapper basename. Older records can use their logical
/// name with a known extension for the declared media type.
pub fn download_filename(original: Option<&str>, logical_name: &str, media_type: &str) -> String {
    if let Some(name) = original.filter(|name| valid_filename(name)) {
        return name.to_owned();
    }
    let name = if valid_filename(logical_name) {
        logical_name
    } else {
        "artifact"
    };
    if name.trim_start_matches('.').contains('.') {
        return name.to_owned();
    }
    let suffix = match media_type.to_ascii_lowercase().as_str() {
        "application/fits" | "image/fits" => ".fits",
        "application/gzip" => ".gz",
        "application/json" => ".json",
        "application/pdf" => ".pdf",
        "application/zip" => ".zip",
        "image/gif" => ".gif",
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/tiff" => ".tiff",
        "image/webp" => ".webp",
        "text/csv" => ".csv",
        "text/plain" => ".txt",
        "text/tab-separated-values" => ".tsv",
        _ => "",
    };
    let filename = format!("{name}{suffix}");
    if valid_filename(&filename) {
        filename
    } else {
        format!("artifact{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_names_preserve_safe_basenames_and_fall_back_for_older_artifacts() {
        for name in ["wind.png", "Région nord.csv", "orbit.final.v1.csv"] {
            assert!(valid_filename(name), "{name:?}");
            assert_eq!(download_filename(Some(name), "plot", "image/png"), name);
        }
        for name in [
            "",
            ".",
            "..",
            "../escape.png",
            "parent/file.png",
            "parent\\file.png",
            "AUX.txt",
            "lpt¹.csv",
            "a:b.png",
            "a\r\nb.png",
            "a\"b.png",
            ".enumeray-download-test",
        ] {
            assert!(!valid_filename(name), "{name:?}");
            assert_eq!(
                download_filename(Some(name), "plot", "image/png"),
                "plot.png"
            );
        }
        assert_eq!(download_filename(None, "wind", "text/csv"), "wind.csv");
        assert_eq!(
            download_filename(None, "../escape", "text/csv"),
            "artifact.csv"
        );
        assert_eq!(
            download_filename(None, "data.tgz", "application/gzip"),
            "data.tgz"
        );
    }
}
