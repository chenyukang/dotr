use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use globset::Glob;

use crate::{
    config::{NormalizeFormat, NormalizeRule},
    hash::sha256_bytes,
};

pub fn normalized_sha256_for_file(
    source_root: &Path,
    source: &Path,
    rules: &[NormalizeRule],
) -> Result<Option<String>> {
    normalized_sha256_for_file_contents(source_root, source, source, rules)
}

pub fn normalized_sha256_for_file_contents(
    source_root: &Path,
    source: &Path,
    content_path: &Path,
    rules: &[NormalizeRule],
) -> Result<Option<String>> {
    let Some(rule) = matching_rule(source_root, source, rules)? else {
        return Ok(None);
    };

    let raw = fs::read(content_path)
        .with_context(|| format!("failed to read {}", content_path.display()))?;
    let normalized = normalize_bytes(source, &raw, rule)?;
    Ok(Some(sha256_bytes(&normalized)))
}

fn matching_rule<'a>(
    source_root: &Path,
    source: &Path,
    rules: &'a [NormalizeRule],
) -> Result<Option<&'a NormalizeRule>> {
    for rule in rules {
        if rule_matches(source_root, source, rule)? {
            return Ok(Some(rule));
        }
    }
    Ok(None)
}

fn rule_matches(source_root: &Path, source: &Path, rule: &NormalizeRule) -> Result<bool> {
    let Some(pattern) = rule.match_path.as_deref() else {
        return Ok(source == source_root);
    };

    let relative = source.strip_prefix(source_root).unwrap_or(source);
    if pattern == path_for_match(relative) {
        return Ok(true);
    }

    let matcher = Glob::new(pattern)
        .with_context(|| format!("invalid normalize match pattern {pattern:?}"))?
        .compile_matcher();
    Ok(matcher.is_match(path_for_match(relative)))
}

fn path_for_match(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn normalize_bytes(source: &Path, raw: &[u8], rule: &NormalizeRule) -> Result<Vec<u8>> {
    let format = rule
        .format
        .or_else(|| infer_format(source))
        .with_context(|| {
            format!(
                "normalize rule for {} needs an explicit format",
                source.display()
            )
        })?;

    match format {
        NormalizeFormat::Toml => normalize_toml(raw, rule),
        NormalizeFormat::Json => normalize_json(raw, rule),
        NormalizeFormat::Text => {
            if !rule.drop_paths.is_empty() {
                bail!("drop_paths requires a structured normalize format");
            }
            Ok(raw.to_vec())
        }
    }
}

fn infer_format(source: &Path) -> Option<NormalizeFormat> {
    match source.extension().and_then(|extension| extension.to_str()) {
        Some("toml") => Some(NormalizeFormat::Toml),
        Some("json") => Some(NormalizeFormat::Json),
        Some("txt") | Some("conf") => Some(NormalizeFormat::Text),
        _ => None,
    }
}

fn normalize_toml(raw: &[u8], rule: &NormalizeRule) -> Result<Vec<u8>> {
    let raw = std::str::from_utf8(raw).context("TOML normalize input is not UTF-8")?;
    let mut value = raw
        .parse::<toml::Value>()
        .context("failed to parse TOML for normalize")?;
    for path in &rule.drop_paths {
        drop_toml_path(&mut value, &split_drop_path(path));
    }
    toml::to_string(&value)
        .map(String::into_bytes)
        .context("failed to serialize normalized TOML")
}

fn drop_toml_path(value: &mut toml::Value, segments: &[&str]) {
    let Some((head, tail)) = segments.split_first() else {
        return;
    };

    if *head == "*" {
        if let Some(table) = value.as_table_mut() {
            for (_, child) in table.iter_mut() {
                drop_toml_path(child, tail);
            }
        }
        return;
    }

    let Some(table) = value.as_table_mut() else {
        return;
    };

    if tail.is_empty() {
        table.remove(*head);
    } else if let Some(child) = table.get_mut(*head) {
        drop_toml_path(child, tail);
    }
}

fn normalize_json(raw: &[u8], rule: &NormalizeRule) -> Result<Vec<u8>> {
    let mut value =
        serde_json::from_slice::<serde_json::Value>(raw).context("failed to parse JSON")?;
    for path in &rule.drop_paths {
        drop_json_path(&mut value, &split_drop_path(path));
    }
    serde_json::to_vec(&value).context("failed to serialize normalized JSON")
}

fn drop_json_path(value: &mut serde_json::Value, segments: &[&str]) {
    let Some((head, tail)) = segments.split_first() else {
        return;
    };

    if *head == "*" {
        if let Some(object) = value.as_object_mut() {
            for child in object.values_mut() {
                drop_json_path(child, tail);
            }
        }
        return;
    }

    let Some(object) = value.as_object_mut() else {
        return;
    };

    if tail.is_empty() {
        object.remove(*head);
    } else if let Some(child) = object.get_mut(*head) {
        drop_json_path(child, tail);
    }
}

fn split_drop_path(path: &str) -> Vec<&str> {
    path.split('.')
        .filter(|segment| !segment.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(match_path: &str, drop_path: &str) -> NormalizeRule {
        NormalizeRule {
            match_path: Some(match_path.to_string()),
            format: None,
            drop_paths: vec![drop_path.to_string()],
        }
    }

    #[test]
    fn toml_drop_paths_supports_wildcard_segments() {
        let normalized = normalize_bytes(
            Path::new("config.toml"),
            br#"
                [marketplaces.openai-bundled]
                last_updated = "2026-05-30T00:00:00Z"
                source_type = "local"

                [marketplaces.other]
                last_updated = "2026-05-30T00:00:00Z"
                source_type = "remote"
            "#,
            &rule("config.toml", "marketplaces.*.last_updated"),
        )
        .unwrap();
        let normalized = String::from_utf8(normalized).unwrap();

        assert!(!normalized.contains("last_updated"));
        assert!(normalized.contains("source_type = \"local\""));
        assert!(normalized.contains("source_type = \"remote\""));
    }

    #[test]
    fn match_is_relative_to_source_root() {
        let source_root = Path::new("/home/me/.codex");
        let source = Path::new("/home/me/.codex/config.toml");

        assert!(
            matching_rule(
                source_root,
                source,
                &[rule("config.toml", "runtime.updated_at")]
            )
            .unwrap()
            .is_some()
        );
        assert!(
            matching_rule(
                source_root,
                source,
                &[rule("rules/**", "runtime.updated_at")]
            )
            .unwrap()
            .is_none()
        );
    }
}
