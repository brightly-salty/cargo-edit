use super::AnyIndexCache;
use super::Dependency;
use super::RegistrySource;
use super::VersionExt;
use super::errors::*;

/// Query latest version from a registry index
///
/// The registry argument must be specified for crates
/// from alternative registries.
///
/// The latest version will be returned as a `Dependency`. This will fail, when
///
/// - there is no Internet connection and offline is false.
/// - summaries in registry index with an incorrect format.
/// - a crate with the given name does not exist on the registry.
pub fn get_latest_dependency(
    crate_name: &str,
    flag_allow_prerelease: bool,
    rust_version: Option<RustVersion>,
    index: &mut AnyIndexCache,
) -> CargoResult<Dependency> {
    if crate_name.is_empty() {
        anyhow::bail!("Found empty crate name");
    }

    let crate_versions = fuzzy_query_registry_index(crate_name, index)?;

    let dep = read_latest_version(&crate_versions, flag_allow_prerelease, rust_version)?;

    if dep.name != crate_name {
        eprintln!("WARN: Added `{}` instead of `{}`", dep.name, crate_name);
    }

    Ok(dep)
}

/// Find the highest version compatible with a version req
pub fn get_compatible_dependency(
    crate_name: &str,
    version_req: &semver::VersionReq,
    rust_version: Option<RustVersion>,
    index: &mut AnyIndexCache,
) -> CargoResult<Dependency> {
    if crate_name.is_empty() {
        anyhow::bail!("Found empty crate name");
    }

    let crate_versions = fuzzy_query_registry_index(crate_name, index)?;

    let dep = read_compatible_version(&crate_versions, version_req, rust_version)?;

    if dep.name != crate_name {
        eprintln!("WARN: Added `{}` instead of `{}`", dep.name, crate_name);
    }

    Ok(dep)
}

/// Simplified represetation of `package.rust-version`
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RustVersion {
    #[allow(missing_docs)]
    pub major: u64,
    #[allow(missing_docs)]
    pub minor: u64,
    #[allow(missing_docs)]
    pub patch: u64,
}

impl RustVersion {
    /// Minimum-possible `package.rust-version`
    pub const MIN: Self = RustVersion {
        major: 1,
        minor: 0,
        patch: 0,
    };
    /// Maximum-possible `package.rust-version`
    pub const MAX: Self = RustVersion {
        major: u64::MAX,
        minor: u64::MAX,
        patch: u64::MAX,
    };
}

impl std::str::FromStr for RustVersion {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let version_req = text.parse::<semver::VersionReq>()?;
        anyhow::ensure!(
            version_req.comparators.len() == 1,
            "rust-version must be a value like `1.32`"
        );
        let comp = &version_req.comparators[0];
        anyhow::ensure!(
            comp.op == semver::Op::Caret,
            "rust-version must be a value like `1.32`"
        );
        anyhow::ensure!(
            comp.pre == semver::Prerelease::EMPTY,
            "rust-version must be a value like `1.32`"
        );
        Ok(Self {
            major: comp.major,
            minor: comp.minor.unwrap_or(0),
            patch: comp.patch.unwrap_or(0),
        })
    }
}

impl From<&'_ semver::VersionReq> for RustVersion {
    fn from(version_req: &semver::VersionReq) -> Self {
        // HACK: `rust-version` is a subset of the `VersionReq` syntax that only ever
        // has one comparator with a required minor and optional patch, and uses no
        // other features. If in the future this syntax is expanded, this code will need
        // to be updated.
        assert!(version_req.comparators.len() == 1);
        let comp = &version_req.comparators[0];
        assert_eq!(comp.op, semver::Op::Caret);
        assert_eq!(comp.pre, semver::Prerelease::EMPTY);
        Self {
            major: comp.major,
            minor: comp.minor.unwrap_or(0),
            patch: comp.patch.unwrap_or(0),
        }
    }
}

impl From<&'_ semver::Version> for RustVersion {
    fn from(version: &semver::Version) -> Self {
        Self {
            major: version.major,
            minor: version.minor,
            patch: version.patch,
        }
    }
}

#[derive(Debug)]
struct CrateVersion {
    name: String,
    version: semver::Version,
    rust_version: Option<RustVersion>,
    yanked: bool,
}

/// Fuzzy query crate from registry index
fn fuzzy_query_registry_index(
    crate_name: impl Into<String>,
    index: &mut AnyIndexCache,
) -> CargoResult<Vec<CrateVersion>> {
    let crate_name = crate_name.into();
    let mut names = gen_fuzzy_crate_names(crate_name.clone())?;
    if let Some(index) = names.iter().position(|x| *x == crate_name) {
        // ref: https://github.com/killercup/cargo-edit/pull/317#discussion_r307365704
        names.swap(index, 0);
    }

    for the_name in names {
        let krate = match index.krate(&the_name) {
            Ok(Some(krate)) => krate,
            _ => continue,
        };
        return krate
            .versions
            .iter()
            .map(|v| {
                Ok(CrateVersion {
                    name: v.name.to_string(),
                    version: v.version.as_str().parse()?,
                    rust_version: v.rust_version.as_ref().map(|r| r.parse()).transpose()?,
                    yanked: v.yanked,
                })
            })
            .collect();
    }
    Err(no_crate_err(crate_name))
}

/// Generate all similar crate names
///
/// Examples:
///
/// | input | output |
/// | ----- | ------ |
/// | cargo | cargo  |
/// | cargo-edit | cargo-edit, cargo_edit |
/// | parking_lot_core | parking_lot_core, parking_lot-core, parking-lot_core, parking-lot-core |
fn gen_fuzzy_crate_names(crate_name: String) -> CargoResult<Vec<String>> {
    const PATTERN: [u8; 2] = [b'-', b'_'];

    let wildcard_indexs = crate_name
        .bytes()
        .enumerate()
        .filter(|(_, item)| PATTERN.contains(item))
        .map(|(index, _)| index)
        .take(10)
        .collect::<Vec<usize>>();
    if wildcard_indexs.is_empty() {
        return Ok(vec![crate_name]);
    }

    let mut result = vec![];
    let mut bytes = crate_name.into_bytes();
    for mask in 0..2u128.pow(wildcard_indexs.len() as u32) {
        for (mask_index, wildcard_index) in wildcard_indexs.iter().enumerate() {
            let mask_value = (mask >> mask_index) & 1 == 1;
            if mask_value {
                bytes[*wildcard_index] = b'-';
            } else {
                bytes[*wildcard_index] = b'_';
            }
        }
        result.push(String::from_utf8(bytes.clone()).unwrap());
    }
    Ok(result)
}

// Checks whether a version object is a stable release
fn version_is_stable(version: &CrateVersion) -> bool {
    !version.version.is_prerelease()
}

/// Read latest version from Versions structure
fn read_latest_version(
    versions: &[CrateVersion],
    flag_allow_prerelease: bool,
    rust_version: Option<RustVersion>,
) -> CargoResult<Dependency> {
    let latest = versions
        .iter()
        .filter(|&v| flag_allow_prerelease || version_is_stable(v))
        .filter(|&v| !v.yanked)
        .filter(|&v| {
            rust_version
                .and_then(|rust_version| {
                    v.rust_version
                        .map(|v_rust_version| v_rust_version <= rust_version)
                })
                .unwrap_or(true)
        })
        .max_by_key(|&v| v.version.clone())
        .ok_or_else(|| {
            anyhow::format_err!(
                "No available versions exist. Either all were yanked \
                         or only prerelease versions exist. Trying with the \
                         --allow-prerelease flag might solve the issue."
            )
        })?;

    let name = &latest.name;
    let version = latest.version.to_string();
    Ok(Dependency::new(name).set_source(RegistrySource::new(version)))
}

fn read_compatible_version(
    versions: &[CrateVersion],
    version_req: &semver::VersionReq,
    rust_version: Option<RustVersion>,
) -> CargoResult<Dependency> {
    let latest = versions
        .iter()
        .filter(|&v| version_req.matches(&v.version))
        .filter(|&v| !v.yanked)
        .filter(|&v| {
            rust_version
                .and_then(|rust_version| {
                    v.rust_version
                        .map(|v_rust_version| v_rust_version <= rust_version)
                })
                .unwrap_or(true)
        })
        .max_by_key(|&v| v.version.clone())
        .ok_or_else(|| {
            anyhow::format_err!(
                "No available versions exist. Either all were yanked \
                         or only prerelease versions exist. Trying with the \
                         --allow-prerelease flag might solve the issue."
            )
        })?;

    let name = &latest.name;
    let version = latest.version.to_string();
    Ok(Dependency::new(name).set_source(RegistrySource::new(version)))
}

#[test]
fn test_gen_fuzzy_crate_names() {
    fn test_helper(input: &str, expect: &[&str]) {
        let mut actual = gen_fuzzy_crate_names(input.to_string()).unwrap();
        actual.sort();

        let mut expect = expect.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        expect.sort();

        assert_eq!(actual, expect);
    }

    test_helper("", &[""]);
    test_helper("-", &["_", "-"]);
    test_helper("DCjanus", &["DCjanus"]);
    test_helper("DC-janus", &["DC-janus", "DC_janus"]);
    test_helper(
        "DC-_janus",
        &["DC__janus", "DC_-janus", "DC-_janus", "DC--janus"],
    );
}

#[test]
fn get_latest_stable_version() {
    let versions = vec![
        CrateVersion {
            name: "foo".into(),
            version: "0.6.0-alpha".parse().unwrap(),
            rust_version: None,
            yanked: false,
        },
        CrateVersion {
            name: "foo".into(),
            version: "0.5.0".parse().unwrap(),
            rust_version: None,
            yanked: false,
        },
    ];
    assert_eq!(
        read_latest_version(&versions, false, None)
            .unwrap()
            .version()
            .unwrap(),
        "0.5.0"
    );
}

#[test]
fn get_latest_unstable_or_stable_version() {
    let versions = vec![
        CrateVersion {
            name: "foo".into(),
            version: "0.6.0-alpha".parse().unwrap(),
            rust_version: None,
            yanked: false,
        },
        CrateVersion {
            name: "foo".into(),
            version: "0.5.0".parse().unwrap(),
            rust_version: None,
            yanked: false,
        },
    ];
    assert_eq!(
        read_latest_version(&versions, true, None)
            .unwrap()
            .version()
            .unwrap(),
        "0.6.0-alpha"
    );
}

#[test]
fn get_latest_version_with_yanked() {
    let versions = vec![
        CrateVersion {
            name: "treexml".into(),
            version: "0.3.1".parse().unwrap(),
            rust_version: None,
            yanked: true,
        },
        CrateVersion {
            name: "true".into(),
            version: "0.3.0".parse().unwrap(),
            rust_version: None,
            yanked: false,
        },
    ];
    assert_eq!(
        read_latest_version(&versions, false, None)
            .unwrap()
            .version()
            .unwrap(),
        "0.3.0"
    );
}

#[test]
fn get_no_latest_version_from_json_when_all_are_yanked() {
    let versions = vec![
        CrateVersion {
            name: "treexml".into(),
            version: "0.3.1".parse().unwrap(),
            rust_version: None,
            yanked: true,
        },
        CrateVersion {
            name: "true".into(),
            version: "0.3.0".parse().unwrap(),
            rust_version: None,
            yanked: true,
        },
    ];
    assert!(read_latest_version(&versions, false, None).is_err());
}
