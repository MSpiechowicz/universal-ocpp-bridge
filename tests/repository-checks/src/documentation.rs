use std::{fs, path::Path};

pub(crate) fn check(root: &Path, errors: &mut Vec<String>) {
    scan_directory(root, root, errors);
}

fn scan_directory(root: &Path, directory: &Path, errors: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(directory) else {
        errors.push(format!(
            "could not read documentation directory {}",
            directory.display()
        ));
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".git" | "target")
            ) {
                scan_directory(root, &path, errors);
            }
        } else if path.extension().is_some_and(|extension| extension == "md") {
            check_markdown(root, &path, errors);
        }
    }
}

fn check_markdown(root: &Path, path: &Path, errors: &mut Vec<String>) {
    let Ok(source) = fs::read_to_string(path) else {
        errors.push(format!(
            "could not read Markdown file {}",
            display_path(root, path).display()
        ));
        return;
    };

    let mut fenced = false;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if line.ends_with(' ') || line.ends_with('\t') {
            errors.push(format!(
                "Markdown trailing whitespace in {}:{}",
                display_path(root, path).display(),
                index + 1
            ));
        }
        if !fenced {
            check_inline_links(root, path, index + 1, line, errors);
        }
    }
}

fn check_inline_links(
    root: &Path,
    source_path: &Path,
    line_number: usize,
    line: &str,
    errors: &mut Vec<String>,
) {
    let mut remaining = line;
    while let Some(link_start) = remaining.find("](") {
        remaining = &remaining[link_start + 2..];
        let Some(link_end) = remaining.find(')') else {
            break;
        };
        let raw_target = &remaining[..link_end];
        remaining = &remaining[link_end + 1..];

        let target = raw_target
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_matches(['<', '>']);
        if target.is_empty() || is_external_or_anchor(target) {
            continue;
        }
        let file_target = target.split(['#', '?']).next().unwrap_or_default();
        if file_target.is_empty() {
            continue;
        }
        let resolved = source_path.parent().unwrap_or(root).join(file_target);
        if !resolved.exists() {
            errors.push(format!(
                "broken local Markdown link {target:?} in {}:{line_number}",
                display_path(root, source_path).display()
            ));
        }
    }
}
fn is_external_or_anchor(target: &str) -> bool {
    target.starts_with('#')
        || target.starts_with("https://")
        || target.starts_with("http://")
        || target.starts_with("mailto:")
}

fn display_path<'a>(root: &'a Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::{check_inline_links, is_external_or_anchor};
    use std::path::Path;

    #[test]
    fn external_links_and_anchors_are_not_local_files() {
        assert!(is_external_or_anchor("https://example.com/docs"));
        assert!(is_external_or_anchor("#local-heading"));
        assert!(!is_external_or_anchor("../README.md#workspace"));
    }

    #[test]
    fn missing_local_link_is_reported() {
        let root = Path::new("/definitely-missing-uob-root");
        let source = root.join("docs/check.md");
        let mut errors = Vec::new();
        check_inline_links(root, &source, 4, "see [missing](other.md)", &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("docs/check.md:4"));
    }
}
