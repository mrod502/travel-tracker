//! `.env` file loading for the lowest configuration layer.
//!
//! Handled here rather than with the `dotenv` crate on purpose: `dotenv` writes
//! into the process environment, which would make the layering impossible to test
//! (tests would mutate global state and race each other). Keeping the file in a
//! value lets a test hand the resolver an env layer directly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Default env file names, most specific first.
const CANDIDATES: [&str; 2] = [".env", ".env.database"];

/// Variables read from an env file.
#[derive(Debug, Clone, Default)]
pub struct EnvFile {
    values: HashMap<String, String>,
    path: Option<PathBuf>,
}

impl EnvFile {
    /// Load `path` if given, otherwise the first [`CANDIDATES`] file found in
    /// `dir`. Nothing found is not an error: the env layer is optional.
    ///
    /// A file that exists but cannot be read *is* reported, because a permission
    /// problem looks exactly like "no defaults configured" otherwise.
    pub fn load(dir: &Path, path: Option<&Path>) -> Result<Self, String> {
        let Some(path) = resolve_path(dir, path) else {
            return Ok(Self::default());
        };

        let contents = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read env file {}: {}", path.display(), e))?;

        Ok(Self {
            values: parse(&contents),
            path: Some(path),
        })
    }

    /// Build from contents already in hand, for tests.
    pub fn from_str(contents: &str) -> Self {
        Self {
            values: parse(contents),
            path: None,
        }
    }

    /// The file the values came from, when there was one.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
}

fn resolve_path(dir: &Path, explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }
    CANDIDATES
        .iter()
        .map(|name| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Parse `KEY=value` lines.
///
/// Blank lines and `#` comments are skipped, an `export ` prefix is tolerated,
/// and matching single or double quotes are stripped, because every dotenv
/// implementation in the wild does all three. An unquoted value keeps the rest of
/// the line verbatim, which is what makes `DATABASE_URL=postgres://…` with an
/// unquoted password work.
pub fn parse(contents: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }

        values.insert(key.to_string(), unquote(value.trim()));
    }

    values
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

/// The lowest configuration layer: the process environment, then the env file.
///
/// A variable the operator exported wins over the file. That is the one ordering
/// dotenv implementations agree on, and it is the useful direction: `PGPASSWORD=x
/// app monitor` should override a checked-out file without editing it.
#[derive(Debug, Clone, Default)]
pub struct EnvLayer {
    file: EnvFile,
}

impl EnvLayer {
    pub fn new(file: EnvFile) -> Self {
        Self { file }
    }

    /// Build the layer from `dir`, honouring `ENV_FILE` as an explicit path.
    pub fn discover(dir: &Path) -> Result<Self, String> {
        let explicit = std::env::var("ENV_FILE").ok().map(PathBuf::from);
        Ok(Self::new(EnvFile::load(dir, explicit.as_deref())?))
    }

    /// A layer backed by exactly these variables, for tests.
    pub fn from_map(values: HashMap<String, String>) -> Self {
        Self {
            file: EnvFile {
                values,
                path: None,
            },
        }
    }

    /// The file the layer read, when there was one.
    pub fn file_path(&self) -> Option<&Path> {
        self.file.path()
    }

    /// Look up a variable, process environment first.
    ///
    /// An exported-but-empty variable counts as absent: `PGDATABASE= app` is how
    /// people clear a value, and an empty database name is never intended.
    pub fn get(&self, key: &str) -> Option<String> {
        match std::env::var(key) {
            Ok(value) if !value.is_empty() => Some(value),
            _ => self.file.get(key).map(|value| value.to_string()).filter(|value| !value.is_empty()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_assignments_and_skips_noise() {
        let values = parse(
            r#"
# database defaults
PGPORT=5432
   LOG_LEVEL = debug
export DATABASE_URL=postgres://user:pass@db:5432/travel
NO_EQUALS_HERE
=missing-key
"#,
        );

        assert_eq!(values.get("PGPORT").map(String::as_str), Some("5432"));
        assert_eq!(values.get("LOG_LEVEL").map(String::as_str), Some("debug"));
        assert_eq!(
            values.get("DATABASE_URL").map(String::as_str),
            Some("postgres://user:pass@db:5432/travel")
        );
        assert_eq!(values.len(), 3, "unparsable lines are skipped: {values:?}");
    }

    #[test]
    fn quotes_are_stripped_only_when_matching() {
        let values = parse(
            r#"
DOUBLE="quoted value"
SINGLE='quoted value'
MISMATCHED="still quoted
TRAILING=unquoted # not a comment
"#,
        );

        assert_eq!(values.get("DOUBLE").map(String::as_str), Some("quoted value"));
        assert_eq!(values.get("SINGLE").map(String::as_str), Some("quoted value"));
        assert_eq!(values.get("MISMATCHED").map(String::as_str), Some("\"still quoted"));
        assert_eq!(
            values.get("TRAILING").map(String::as_str),
            Some("unquoted # not a comment")
        );
    }

    #[test]
    fn passwords_containing_special_characters_survive() {
        let values = parse("DATABASE_URL=postgres://user:p@ss:w/rd@localhost:7789/db");

        assert_eq!(
            values.get("DATABASE_URL").map(String::as_str),
            Some("postgres://user:p@ss:w/rd@localhost:7789/db")
        );
    }

    #[test]
    fn load_prefers_dotenv_then_the_database_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env.database"), "PGPORT=1111\n").unwrap();

        let loaded = EnvFile::load(dir.path(), None).unwrap();
        assert_eq!(loaded.get("PGPORT"), Some("1111"));
        assert_eq!(loaded.path().unwrap().file_name().unwrap(), ".env.database");

        std::fs::write(dir.path().join(".env"), "PGPORT=2222\n").unwrap();
        let loaded = EnvFile::load(dir.path(), None).unwrap();
        assert_eq!(loaded.get("PGPORT"), Some("2222"), ".env wins");
    }

    #[test]
    fn a_missing_env_file_is_not_an_error_but_an_unreadable_one_is() {
        let dir = tempfile::tempdir().unwrap();
        let absent = EnvFile::load(dir.path(), None).unwrap();
        assert!(absent.path().is_none());
        assert!(absent.get("ANYTHING").is_none());

        let error = EnvFile::load(dir.path(), Some(&dir.path().join("nope"))).unwrap_err();
        assert!(error.contains("nope"), "got: {error}");
    }

    #[test]
    fn an_exported_variable_beats_the_file_and_an_empty_one_is_absent() {
        let file = EnvFile::from_str("UNIQ_TEST_VAR=from-file\nEMPTY_TEST_VAR=from-file\n");
        let env = EnvLayer::new(file);

        std::env::set_var("UNIQ_TEST_VAR", "from-process");
        assert_eq!(env.get("UNIQ_TEST_VAR").as_deref(), Some("from-process"));
        std::env::remove_var("UNIQ_TEST_VAR");
        assert_eq!(env.get("UNIQ_TEST_VAR").as_deref(), Some("from-file"));

        std::env::set_var("EMPTY_TEST_VAR", "");
        assert_eq!(
            env.get("EMPTY_TEST_VAR").as_deref(),
            Some("from-file"),
            "clearing a variable in the shell should fall back to the file"
        );
        std::env::remove_var("EMPTY_TEST_VAR");

        assert_eq!(env.get("NEVER_SET_ANYWHERE"), None);
    }
}
