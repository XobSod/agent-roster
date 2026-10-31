//! Which version a build is, as SemVer: a release's own, or with how far past
//! it the build is and whether it was changed, in the build metadata.
//! `build.rs` works it out (this file is its, here for its tests).

/// The version of a build of a package whose own version (Cargo.toml's) is
/// `package`, from what `git describe --tags --long --dirty --always` said of
/// its checkout, or none without one:
/// - on a release tag, the tag's version: `0.1.0`, or `0.1.0+dirty` if changed;
/// - past it, with how many commits and the commit: `0.1.0+3.g7056428`, and
///   `.dirty` if changed;
/// - with no release tag in reach (a clone without tags, as `cargo install
///   --git` makes), the package's version with the commit: `0.1.0+g7056428`;
/// - without git, the package's version.
pub fn version(package: &str, describe: Option<&str>) -> String {
    let Some(describe) = describe else {
        return package.to_string();
    };
    let (described, dirty) = match describe.strip_suffix("-dirty") {
        Some(described) => (described, true),
        None => (describe, false),
    };
    // `<tag>-<commits since>-g<commit>`, or the commit alone with no tag.
    let mut parts = described.rsplitn(3, '-');
    let past_tag = match (parts.next(), parts.next(), parts.next()) {
        (Some(commit), Some(count), Some(tag)) if commit.starts_with('g') => {
            count.parse::<u64>().ok().map(|count| (tag, count, commit))
        }
        _ => None,
    };
    let (base, mut meta) = match past_tag {
        Some((tag, count, commit)) => match tag.strip_prefix('v').filter(|v| is_release(v)) {
            Some(base) if count == 0 => (base, vec![]),
            Some(base) => (base, vec![count.to_string(), commit.to_string()]),
            None => (package, vec![commit.to_string()]),
        },
        None => (package, vec![format!("g{described}")]),
    };
    if dirty {
        meta.push("dirty".to_string());
    }
    if meta.is_empty() {
        base.to_string()
    } else {
        format!("{base}+{}", meta.join("."))
    }
}

/// Whether a tag's version is a release's: three numbers, maybe a pre-release
/// after them (`1.2.0-rc.1`).
fn is_release(version: &str) -> bool {
    let core = version.split_once('-').map_or(version, |(core, _)| core);
    let numbers: Vec<&str> = core.split('.').collect();
    numbers.len() == 3
        && numbers
            .iter()
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::version;

    #[test]
    fn a_release_is_its_tag() {
        assert_eq!(version("0.1.0", Some("v0.1.0-0-g7056428")), "0.1.0");
        assert_eq!(
            version("0.1.0", Some("v0.1.0-0-g7056428-dirty")),
            "0.1.0+dirty"
        );
        assert_eq!(
            version("1.2.0", Some("v1.2.0-rc.1-0-gabcdef0")),
            "1.2.0-rc.1"
        );
    }

    #[test]
    fn past_a_release_says_how_far_and_from_what() {
        assert_eq!(
            version("0.1.0", Some("v0.1.0-3-g7056428")),
            "0.1.0+3.g7056428"
        );
        assert_eq!(
            version("0.1.0", Some("v0.1.0-3-g7056428-dirty")),
            "0.1.0+3.g7056428.dirty"
        );
        // The release it's past, not a version Cargo.toml went on to before
        // that was released.
        assert_eq!(
            version("0.2.0", Some("v0.1.0-12-g7056428")),
            "0.1.0+12.g7056428"
        );
    }

    #[test]
    fn with_no_release_tag_it_is_the_package_with_its_commit() {
        assert_eq!(version("0.1.0", Some("7056428")), "0.1.0+g7056428");
        assert_eq!(
            version("0.1.0", Some("7056428-dirty")),
            "0.1.0+g7056428.dirty"
        );
        // A tag that isn't a release's.
        assert_eq!(version("0.1.0", Some("v1-2-g7056428")), "0.1.0+g7056428");
        assert_eq!(version("0.1.0", Some("v0.1-0-g7056428")), "0.1.0+g7056428");
        assert_eq!(
            version("0.1.0", Some("v0.1.x-3-g7056428-dirty")),
            "0.1.0+g7056428.dirty"
        );
    }

    #[test]
    fn without_git_it_is_the_package() {
        assert_eq!(version("0.1.0", None), "0.1.0");
    }
}
