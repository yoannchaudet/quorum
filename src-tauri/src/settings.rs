use std::collections::HashSet;
use std::process::Command;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, StoreError};
use crate::state::AppStore;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SettingsDto {
    pub database_path: String,
    pub planning_models: Vec<String>,
    pub implementation_model: String,
    pub adversary_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct UpdateSettingsRequest {
    pub planning_models: Vec<String>,
    pub implementation_model: String,
    pub adversary_model: String,
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
            settings_from_assignments(self.store, assignments)
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
            transaction.commit()?;
            Ok(())
        })?;

        self.get()
    }
}

fn settings_from_assignments(
    store: &AppStore,
    assignments: Vec<(String, String)>,
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
    })
}

fn normalized_planners(models: Vec<String>) -> Result<Vec<String>, AppError> {
    if !(1..=3).contains(&models.len()) {
        return Err(AppError::validation(
            "Choose between one and three planning models.",
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

    use super::{parse_copilot_models, SettingsService, UpdateSettingsRequest};
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

        let updated = service
            .update(UpdateSettingsRequest {
                planning_models: vec![" custom-planner ".to_owned()],
                implementation_model: "implementation".to_owned(),
                adversary_model: "adversary".to_owned(),
            })
            .expect("update");
        assert_eq!(updated.planning_models, ["custom-planner"]);
        assert_eq!(service.get().expect("reload"), updated);

        service
            .update(UpdateSettingsRequest {
                planning_models: vec![],
                implementation_model: "changed".to_owned(),
                adversary_model: "changed".to_owned(),
            })
            .expect_err("invalid");
        assert_eq!(service.get().expect("unchanged"), updated);
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
