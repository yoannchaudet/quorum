use std::collections::HashSet;
use std::process::Command;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, StoreError};
use crate::state::AppStore;

pub const DEFAULT_TERMINAL_APPLICATION: &str = "Ghostty.app";
pub const DEFAULT_TERMINAL_ARGUMENTS: &str =
    "-W -na {terminalApplication} --args -e copilot -C \"{repositoryPath}\" --resume={sessionName}";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SettingsDto {
    pub database_path: String,
    pub planning_models: Vec<String>,
    pub implementation_model: String,
    pub adversary_model: String,
    pub terminal_application: String,
    pub terminal_arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct UpdateSettingsRequest {
    pub planning_models: Vec<String>,
    pub implementation_model: String,
    pub adversary_model: String,
    pub terminal_application: String,
    pub terminal_arguments: String,
}

pub struct SettingsService<'a> {
    store: &'a AppStore,
}

impl<'a> SettingsService<'a> {
    pub const fn new(store: &'a AppStore) -> Self {
        Self { store }
    }

    pub fn get(&self) -> Result<SettingsDto, AppError> {
        self.store.with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT role, model_id FROM model_assignments ORDER BY role, position")?;
            let assignments = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let terminal_application = setting(connection, "terminal_application")?;
            let terminal_arguments = setting(connection, "terminal_arguments")?;
            settings_from_assignments(
                self.store,
                assignments,
                terminal_application,
                terminal_arguments,
            )
        })
    }

    pub fn update(&self, request: UpdateSettingsRequest) -> Result<SettingsDto, AppError> {
        let planning_models = normalized_planners(request.planning_models)?;
        let implementation_model = normalized_required(
            &request.implementation_model,
            "Choose an implementation model.",
        )?;
        let adversary_model =
            normalized_required(&request.adversary_model, "Choose an adversary model.")?;
        let terminal_application = normalized_required(
            &request.terminal_application,
            "Choose a terminal application.",
        )?;
        let terminal_arguments = validate_terminal_arguments(&request.terminal_arguments)?;

        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute("DELETE FROM model_assignments", [])?;
            for (position, model) in planning_models.iter().enumerate() {
                transaction.execute(
                    "INSERT INTO model_assignments (role, position, model_id) VALUES ('planner', ?1, ?2)",
                    params![position, model],
                )?;
            }
            transaction.execute(
                "INSERT INTO model_assignments (role, position, model_id) VALUES ('implementation', 0, ?1)",
                [&implementation_model],
            )?;
            transaction.execute(
                "INSERT INTO model_assignments (role, position, model_id) VALUES ('adversary', 0, ?1)",
                [&adversary_model],
            )?;
            transaction.execute(
                "INSERT INTO app_settings (key, value) VALUES ('terminal_application', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [&terminal_application],
            )?;
            transaction.execute(
                "INSERT INTO app_settings (key, value) VALUES ('terminal_arguments', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [&terminal_arguments],
            )?;
            transaction.commit()?;
            Ok(())
        })?;

        self.get()
    }
}

fn settings_from_assignments(
    store: &AppStore,
    assignments: Vec<(String, String)>,
    terminal_application: String,
    terminal_arguments: String,
) -> Result<SettingsDto, StoreError> {
    let mut planning_models = Vec::new();
    let mut implementation_model = None;
    let mut adversary_model = None;
    for (role, model) in assignments {
        match role.as_str() {
            "planner" => planning_models.push(model),
            "implementation" => implementation_model = Some(model),
            "adversary" => adversary_model = Some(model),
            _ => {
                return Err(AppError::database(format!(
                    "Quorum contains an unknown model role: {role}"
                ))
                .into());
            }
        }
    }
    if planning_models.is_empty() || planning_models.len() > 3 {
        return Err(
            AppError::database("Quorum's planning model configuration is incomplete.").into(),
        );
    }
    Ok(SettingsDto {
        database_path: store.database_path().to_string_lossy().into_owned(),
        planning_models,
        implementation_model: implementation_model.ok_or_else(|| {
            AppError::database("Quorum's implementation model configuration is missing.")
        })?,
        adversary_model: adversary_model.ok_or_else(|| {
            AppError::database("Quorum's adversary model configuration is missing.")
        })?,
        terminal_application,
        terminal_arguments,
    })
}

fn setting(connection: &rusqlite::Connection, key: &str) -> Result<String, StoreError> {
    if let Some(value) = connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?
    {
        return Ok(value);
    }
    let default = match key {
        "terminal_application" => DEFAULT_TERMINAL_APPLICATION,
        "terminal_arguments" => DEFAULT_TERMINAL_ARGUMENTS,
        _ => {
            return Err(
                AppError::database(format!("Quorum is missing required setting {key}.")).into(),
            );
        }
    };
    connection.execute(
        "INSERT INTO app_settings (key, value) VALUES (?1, ?2)",
        params![key, default],
    )?;
    Ok(default.to_owned())
}

fn normalized_planners(models: Vec<String>) -> Result<Vec<String>, AppError> {
    if !(2..=3).contains(&models.len()) {
        return Err(AppError::validation(
            "Choose between two and three planning models.",
        ));
    }
    models
        .into_iter()
        .map(|model| normalized_required(&model, "Planning models cannot be empty."))
        .collect()
}

fn normalized_required(model: &str, message: &str) -> Result<String, AppError> {
    let model = model.trim();
    if model.is_empty() {
        Err(AppError::validation(message))
    } else {
        Ok(model.to_owned())
    }
}

pub fn expand_terminal_arguments(
    template: &str,
    terminal_application: &str,
    repository_path: &str,
    session_name: &str,
) -> Result<Vec<String>, AppError> {
    let template = validate_terminal_arguments(template)?;
    shell_words::split(&template)
        .map_err(|error| AppError::validation(format!("Terminal arguments are invalid: {error}")))
        .map(|arguments| {
            arguments
                .into_iter()
                .map(|argument| {
                    argument
                        .replace("{terminalApplication}", terminal_application)
                        .replace("{repositoryPath}", repository_path)
                        .replace("{sessionName}", session_name)
                })
                .collect()
        })
}

fn validate_terminal_arguments(template: &str) -> Result<String, AppError> {
    let template = normalized_required(template, "Enter terminal launch arguments.")?;
    let allowed = ["terminalApplication", "repositoryPath", "sessionName"];
    let mut found = HashSet::new();
    let mut remainder = template.as_str();
    while let Some(position) = remainder.find(['{', '}']) {
        if remainder.as_bytes()[position] == b'}' {
            return Err(AppError::validation(
                "Terminal arguments contain an unmatched closing brace.",
            ));
        }
        let after_open = &remainder[position + 1..];
        let close = after_open.find('}').ok_or_else(|| {
            AppError::validation("Terminal arguments contain an unclosed placeholder.")
        })?;
        let name = &after_open[..close];
        if name.contains('{') || !allowed.contains(&name) {
            return Err(AppError::validation(format!(
                "Terminal arguments contain unsupported placeholder {{{name}}}."
            )));
        }
        found.insert(name);
        remainder = &after_open[close + 1..];
    }
    for placeholder in allowed {
        if !found.contains(placeholder) {
            return Err(AppError::validation(format!(
                "Terminal arguments must include {{{placeholder}}}."
            )));
        }
    }
    shell_words::split(&template).map_err(|error| {
        AppError::validation(format!("Terminal arguments are invalid: {error}"))
    })?;
    Ok(template)
}

pub fn discover_copilot_models() -> Result<Vec<String>, AppError> {
    let output = Command::new("copilot")
        .args(["help", "config"])
        .output()
        .map_err(|error| {
            AppError::external(format!("Copilot CLI could not be started: {error}"))
        })?;
    if !output.status.success() {
        return Err(AppError::external(format!(
            "Copilot CLI model discovery failed with status {}.",
            output.status
        )));
    }
    let help = String::from_utf8(output.stdout)
        .map_err(|_| AppError::external("Copilot CLI returned unreadable help output."))?;
    parse_copilot_models(&help)
}

fn parse_copilot_models(help: &str) -> Result<Vec<String>, AppError> {
    let mut in_models = false;
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for line in help.lines() {
        if !in_models {
            in_models = line.trim_start().starts_with("`model`:");
            continue;
        }
        let trimmed = line.trim();
        let Some(model) = trimmed
            .strip_prefix("- \"")
            .and_then(|value| value.strip_suffix('"'))
        else {
            if !models.is_empty() {
                break;
            }
            continue;
        };
        if seen.insert(model.to_owned()) {
            models.push(model.to_owned());
        }
    }
    if models.is_empty() {
        Err(AppError::external(
            "Copilot CLI did not advertise any available models.",
        ))
    } else {
        Ok(models)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{
        expand_terminal_arguments, parse_copilot_models, SettingsService, UpdateSettingsRequest,
        DEFAULT_TERMINAL_APPLICATION, DEFAULT_TERMINAL_ARGUMENTS,
    };
    use crate::state::AppStore;

    #[test]
    fn loads_defaults_and_atomically_replaces_assignments() {
        let directory = tempdir().expect("temp dir");
        let store = AppStore::open(directory.path()).expect("store");
        let service = SettingsService::new(&store);
        let defaults = service.get().expect("defaults");
        assert_eq!(defaults.planning_models, ["gpt-5.6-sol", "claude-opus-5"]);
        assert_eq!(defaults.implementation_model, "gpt-5.6-sol");
        assert_eq!(defaults.adversary_model, "claude-opus-5");
        assert_eq!(defaults.terminal_application, DEFAULT_TERMINAL_APPLICATION);
        assert_eq!(defaults.terminal_arguments, DEFAULT_TERMINAL_ARGUMENTS);

        let updated = service
            .update(UpdateSettingsRequest {
                planning_models: vec![
                    " custom-planner ".to_owned(),
                    " second-planner ".to_owned(),
                ],
                implementation_model: "implementation".to_owned(),
                adversary_model: "adversary".to_owned(),
                terminal_application: "Warp.app".to_owned(),
                terminal_arguments:
                    "-W -na {terminalApplication} --args copilot -C {repositoryPath} --resume={sessionName}"
                        .to_owned(),
            })
            .expect("update");
        assert_eq!(
            updated.planning_models,
            ["custom-planner", "second-planner"]
        );
        assert_eq!(service.get().expect("reload"), updated);

        service
            .update(UpdateSettingsRequest {
                planning_models: vec!["only-one-planner".to_owned()],
                implementation_model: "changed".to_owned(),
                adversary_model: "changed".to_owned(),
                terminal_application: "changed".to_owned(),
                terminal_arguments: "changed".to_owned(),
            })
            .expect_err("invalid");
        assert_eq!(service.get().expect("unchanged"), updated);
    }

    #[test]
    fn safely_expands_terminal_argument_placeholders() {
        let arguments = expand_terminal_arguments(
            "-W -na {terminalApplication} --args -e copilot -C \"{repositoryPath}\" --resume={sessionName}",
            "Ghostty.app",
            "/Users/example/repository with spaces",
            "quorum-work-run-planner-1-abcd",
        )
        .expect("arguments");
        assert_eq!(
            arguments,
            [
                "-W",
                "-na",
                "Ghostty.app",
                "--args",
                "-e",
                "copilot",
                "-C",
                "/Users/example/repository with spaces",
                "--resume=quorum-work-run-planner-1-abcd"
            ]
        );
    }

    #[test]
    fn rejects_unknown_or_malformed_terminal_placeholders() {
        for template in [
            "-W {terminalApplication} {repositoryPath} {session}",
            "-W {terminalApplication} {repositoryPath} {sessionName",
            "-W {terminalApplication} {repositoryPath}} {sessionName}",
        ] {
            assert_eq!(
                expand_terminal_arguments(template, "Ghostty.app", "/repo", "session")
                    .expect_err("invalid placeholder")
                    .code,
                "validation"
            );
        }
    }

    #[test]
    fn parses_only_the_model_setting_and_preserves_order() {
        let help = r#"
  `model`: AI model to use.
    - "gpt-5.6-sol"
    - "claude-opus-5"
    - "gpt-5.6-sol"

  `contextTier`: context.
    - "not-a-model"
"#;
        assert_eq!(
            parse_copilot_models(help).expect("models"),
            ["gpt-5.6-sol", "claude-opus-5"]
        );
        assert_eq!(
            parse_copilot_models("`model`: missing")
                .expect_err("missing")
                .code,
            "external"
        );
    }
}
