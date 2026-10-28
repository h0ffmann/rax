//! Validation the API server applies before saving a resource.
//!
//! A resource's name and atespace become Substrate resource names (the task's
//! actor and ActorTemplate, and the atespace itself), which must be lowercase
//! RFC 1123 labels. Checking them at apply time turns an asynchronous
//! ActorCreationFailed condition into an immediate error.

use std::sync::LazyLock;

use regex::Regex;

use crate::v1alpha1::{Model, ObjectMeta, Task, TaskSpec, Workspace};

/// A problem that makes a resource unusable, phrased for the person who wrote
/// the manifest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ValidationError(String);

impl ValidationError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }

    pub(crate) fn prefixed(self, field: &str) -> Self {
        Self(format!("{field}: {}", self.0))
    }
}

/// The longest name or atespace a resource may have.
pub const MAX_NAME_LENGTH: usize = 63;

/// Where the task's workspace is mounted unless the binding sets a path. It is
/// the task command's working directory, and a task with no workspace gets an
/// empty directory there.
pub const DEFAULT_WORKSPACE_PATH: &str = "/workspace";

/// The longest environment variable value Substrate accepts (EnvVar.value in
/// ateapi.proto caps it at 32,768 characters), applied to the byte length.
pub const MAX_ENV_VALUE_SIZE: usize = 32 << 10;

/// The largest a task's launch configuration may be once encoded as YAML, the
/// form in which it reaches the runner through `AX_TASK_YAML`. It keeps 1 KiB
/// of [`MAX_ENV_VALUE_SIZE`] in reserve for fields the control plane fills in
/// after validation, such as metadata.atespace and metadata.createTime.
pub const MAX_TASK_YAML_SIZE: usize = MAX_ENV_VALUE_SIZE - (1 << 10);

/// The largest spec.config a task may have, measured as its JSON encoding. It
/// leaves room for the rest of the spec within [`MAX_TASK_YAML_SIZE`] and gives
/// a more specific error than [`validate_task_size`].
pub const MAX_TASK_CONFIG_SIZE: usize = 16 << 10;

static NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9]([-a-z0-9]*[a-z0-9])?$").unwrap());

/// Reports whether `s` can be used as a resource name or atespace: a lowercase
/// RFC 1123 label of at most [`MAX_NAME_LENGTH`] characters.
pub fn validate_name(s: &str) -> Result<(), ValidationError> {
    if s.is_empty() {
        return Err(ValidationError::new("must not be empty"));
    }
    if s.len() > MAX_NAME_LENGTH || !NAME.is_match(s) {
        return Err(ValidationError::new(format!(
            "must be a lowercase RFC 1123 label: at most {MAX_NAME_LENGTH} lowercase alphanumeric characters or '-', starting and ending with an alphanumeric character"
        )));
    }
    Ok(())
}

/// Checks a resource's name and, when set, its atespace. An empty atespace is
/// fine here; the API server defaults it.
pub fn validate_object_meta(meta: Option<&ObjectMeta>) -> Result<(), ValidationError> {
    let (name, atespace) = meta.map_or(("", ""), |m| (m.name.as_str(), m.atespace.as_str()));
    validate_name(name)
        .map_err(|e| e.prefixed(&format!("metadata.name: invalid value {name:?}")))?;
    if !atespace.is_empty() {
        validate_name(atespace)
            .map_err(|e| e.prefixed(&format!("metadata.atespace: invalid value {atespace:?}")))?;
    }
    Ok(())
}

/// Reports the first problem with a workspace that would make it unusable.
pub fn validate_workspace(w: &Workspace) -> Result<(), ValidationError> {
    validate_object_meta(w.metadata.as_ref())?;
    if let Some(spec) = &w.spec {
        for (i, f) in spec.files.iter().enumerate() {
            if f.path.is_empty() {
                return Err(ValidationError::new(format!(
                    "spec.files[{i}]: path is required"
                )));
            }
        }
    }
    Ok(())
}

/// Reports the first problem with a model that would make it unusable.
pub fn validate_model(m: &Model) -> Result<(), ValidationError> {
    validate_object_meta(m.metadata.as_ref())
}

/// Reports the first problem with a task that would make it impossible to run
/// correctly.
pub fn validate_task(t: &Task) -> Result<(), ValidationError> {
    validate_object_meta(t.metadata.as_ref())?;
    let spec = t.spec.as_ref();
    if let Some(r) = spec.and_then(|s| s.workspace.as_ref()) {
        if r.name.is_empty() {
            return Err(ValidationError::new("spec.workspace: name is required"));
        }
        if !r.path.is_empty() && !r.path.starts_with('/') {
            return Err(ValidationError::new(format!(
                "spec.workspace: path {:?} must be absolute",
                r.path
            )));
        }
    }
    if let Some(spec) = spec {
        validate_task_config(spec)?;
    }
    validate_task_size(t)?;
    crate::validate_resources(spec.and_then(|s| s.resources.as_ref()))
}

/// Checks that the task's launch configuration, encoded as the control plane
/// encodes it for `AX_TASK_YAML`, fits within [`MAX_TASK_YAML_SIZE`]. Larger
/// tasks would be rejected by Substrate when their template is created.
pub fn validate_task_size(t: &Task) -> Result<(), ValidationError> {
    let launch = Task {
        status: None,
        ..t.clone()
    };
    let data =
        crate::to_yaml(&launch).map_err(|e| ValidationError::new(format!("encoding task: {e}")))?;
    if data.len() > MAX_TASK_YAML_SIZE {
        return Err(ValidationError::new(format!(
            "task: encoded launch configuration is {} bytes, which exceeds the limit of {MAX_TASK_YAML_SIZE} bytes",
            data.len()
        )));
    }
    Ok(())
}

/// Checks that spec.config fits within [`MAX_TASK_CONFIG_SIZE`]. The config is
/// otherwise opaque: its schema belongs to the runner.
pub fn validate_task_config(spec: &TaskSpec) -> Result<(), ValidationError> {
    let Some(config) = &spec.config else {
        return Ok(());
    };
    let data = serde_json::to_vec(config)
        .map_err(|e| ValidationError::new(format!("spec.config: {e}")))?;
    if data.len() > MAX_TASK_CONFIG_SIZE {
        return Err(ValidationError::new(format!(
            "spec.config: {} bytes exceeds the limit of {MAX_TASK_CONFIG_SIZE} bytes",
            data.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1alpha1::{EnvVar, File, WorkspaceRef, WorkspaceSpec};

    fn meta(name: &str, atespace: &str) -> Option<ObjectMeta> {
        Some(ObjectMeta {
            name: name.into(),
            atespace: atespace.into(),
            ..Default::default()
        })
    }

    pub(crate) fn config(json: serde_json::Value) -> pbjson_types::Struct {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn names() {
        let long_ok = "a".repeat(63);
        for name in [
            "a",
            "0",
            "task123",
            "my-task",
            "a-b-c",
            "123-abc",
            long_ok.as_str(),
        ] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
        let long_bad = "a".repeat(64);
        for name in [
            "",
            "Task-With-Caps",
            "my_task",
            "my.task",
            "-leading-dash",
            "trailing-dash-",
            "with space",
            "tab\tchar",
            "ünïcödé",
            "slash/name",
            long_bad.as_str(),
        ] {
            assert!(validate_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn object_meta() {
        let cases: Vec<(&str, Option<ObjectMeta>, &str)> = vec![
            ("nil metadata", None, "metadata.name"),
            (
                "missing name",
                meta("", "default"),
                r#"metadata.name: invalid value """#,
            ),
            ("name only", meta("task123", ""), ""),
            ("name and atespace", meta("task123", "team-a"), ""),
            (
                "uppercase name",
                meta("Task-With-Caps", ""),
                r#"metadata.name: invalid value "Task-With-Caps""#,
            ),
            ("underscore in name", meta("my_task", ""), "metadata.name"),
            ("name too long", meta(&"x".repeat(64), ""), "metadata.name"),
            (
                "uppercase atespace",
                meta("task123", "Default"),
                r#"metadata.atespace: invalid value "Default""#,
            ),
            (
                "dotted atespace",
                meta("task123", "team.a"),
                "metadata.atespace",
            ),
        ];
        for (name, m, want) in cases {
            match validate_object_meta(m.as_ref()) {
                Ok(()) => assert!(
                    want.is_empty(),
                    "{name}: expected error containing {want:?}"
                ),
                Err(e) => assert!(
                    !want.is_empty() && e.to_string().contains(want),
                    "{name}: {e}"
                ),
            }
        }
        assert!(
            validate_task(&Task {
                metadata: meta("Bad", ""),
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            validate_workspace(&Workspace {
                metadata: meta("Bad", ""),
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            validate_model(&Model {
                metadata: meta("Bad", ""),
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn tasks() {
        let ws = |name: &str, path: &str| {
            Some(WorkspaceRef {
                name: name.into(),
                path: path.into(),
            })
        };
        let blob = |n: usize| config(serde_json::json!({ "blob": "x".repeat(n) }));
        let cases: Vec<(&str, Option<TaskSpec>, &str)> = vec![
            ("nil spec", None, ""),
            ("no workspace", Some(TaskSpec::default()), ""),
            (
                "default path",
                Some(TaskSpec {
                    workspace: ws("a", ""),
                    ..Default::default()
                }),
                "",
            ),
            (
                "explicit path",
                Some(TaskSpec {
                    workspace: ws("a", "/x"),
                    ..Default::default()
                }),
                "",
            ),
            (
                "nameless binding",
                Some(TaskSpec {
                    workspace: ws("", "/w"),
                    ..Default::default()
                }),
                "spec.workspace: name is required",
            ),
            (
                "relative path",
                Some(TaskSpec {
                    workspace: ws("a", "rel"),
                    ..Default::default()
                }),
                r#"spec.workspace: path "rel" must be absolute"#,
            ),
            (
                "config",
                Some(TaskSpec {
                    config: Some(config(
                        serde_json::json!({"suite": "swe", "shard": {"index": 7}}),
                    )),
                    ..Default::default()
                }),
                "",
            ),
            (
                "oversized config",
                Some(TaskSpec {
                    config: Some(blob(MAX_TASK_CONFIG_SIZE)),
                    ..Default::default()
                }),
                "spec.config:",
            ),
            (
                "config near the limit",
                Some(TaskSpec {
                    config: Some(blob(MAX_TASK_CONFIG_SIZE - 100)),
                    ..Default::default()
                }),
                "",
            ),
            (
                // Each value fits on its own, but together they do not fit in
                // one Substrate environment variable.
                "oversized task",
                Some(TaskSpec {
                    config: Some(blob(MAX_TASK_CONFIG_SIZE - 100)),
                    env: vec![EnvVar {
                        name: "BIG".into(),
                        value: "y".repeat(16 << 10),
                    }],
                    ..Default::default()
                }),
                "task: encoded launch configuration",
            ),
            (
                "oversized command",
                Some(TaskSpec {
                    command: vec!["sh".into(), "-c".into(), "z".repeat(MAX_ENV_VALUE_SIZE)],
                    ..Default::default()
                }),
                "task: encoded launch configuration",
            ),
        ];
        for (name, spec, want) in cases {
            let task = Task {
                metadata: meta("task", ""),
                spec,
                ..Default::default()
            };
            match validate_task(&task) {
                Ok(()) => assert!(
                    want.is_empty(),
                    "{name}: expected error containing {want:?}"
                ),
                Err(e) => assert!(
                    !want.is_empty() && e.to_string().contains(want),
                    "{name}: {e}"
                ),
            }
        }
    }

    #[test]
    fn workspace_files_need_a_path() {
        let ws = Workspace {
            metadata: meta("valid-ws", ""),
            spec: Some(WorkspaceSpec {
                files: vec![File {
                    path: "".into(),
                    content: "missing path".into(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = validate_workspace(&ws).unwrap_err().to_string();
        assert!(err.contains("spec.files[0]: path is required"), "{err}");
    }

    #[test]
    fn workspace_path() {
        assert_eq!(TaskSpec::default().workspace_path(), DEFAULT_WORKSPACE_PATH);
        let with = |path: &str| TaskSpec {
            workspace: Some(WorkspaceRef {
                name: "one".into(),
                path: path.into(),
            }),
            ..Default::default()
        };
        assert_eq!(with("").workspace_path(), DEFAULT_WORKSPACE_PATH);
        assert_eq!(with("/opt/code").workspace_path(), "/opt/code");
    }
}
