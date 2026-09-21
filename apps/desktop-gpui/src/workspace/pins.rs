use std::sync::Arc;

use desktop_runtime::SessionId;
use serde_json::{Value, json};

use super::navigation::Route;

pub fn decode(raw: &str) -> Result<Vec<Route>, serde_json::Error> {
    let rows: Vec<Value> = serde_json::from_str(raw)?;
    let mut routes = Vec::new();
    for row in rows {
        let id = row["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(Arc::<str>::from);
        let route = match row["type"].as_str() {
            Some("sessions") => id.map(|id| Route::Session(SessionId(id))),
            Some("humans") => id.map(Route::Human),
            Some("organizations") => id.map(Route::Organization),
            Some("edit") => id.map(Route::Edit),
            Some("contacts") => Some(Route::Contacts),
            Some("templates") => Some(Route::Templates),
            Some("automations") => Some(Route::Automations),
            Some("folders") => Some(Route::Folders),
            Some("calendar") => Some(Route::Calendar),
            Some("changelog") => Some(Route::Changelog),
            Some("onboarding") => Some(Route::Onboarding),
            Some("settings") => Some(Route::settings(
                row["state"]["tab"].as_str().unwrap_or("app"),
            )),
            Some("ai") => Some(Route::settings(
                row["state"]["tab"].as_str().unwrap_or("transcription"),
            )),
            _ => None,
        };
        if let Some(route) = route
            && route.persistent_pin()
            && !routes
                .iter()
                .any(|existing: &Route| existing.same_resource(&route))
        {
            routes.push(route);
        }
    }
    Ok(routes)
}

pub fn encode(routes: &[Route]) -> Result<String, serde_json::Error> {
    let rows = routes
        .iter()
        .filter(|route| route.persistent_pin())
        .map(|route| {
            let (kind, id) = match route {
                Route::Session(SessionId(id)) => ("sessions", Some(id)),
                Route::Human(id) => ("humans", Some(id)),
                Route::Organization(id) => ("organizations", Some(id)),
                Route::Edit(id) => ("edit", Some(id)),
                Route::Contacts => ("contacts", None),
                Route::Templates => ("templates", None),
                Route::Automations => ("automations", None),
                Route::Folders => ("folders", None),
                Route::Calendar => ("calendar", None),
                Route::Changelog => ("changelog", None),
                Route::Settings(_) => ("settings", None),
                Route::Onboarding => ("onboarding", None),
                _ => unreachable!("persistent_pin allowlist"),
            };
            let mut value = json!({"type":kind,"pinned":true});
            if let Some(id) = id {
                value["id"] = json!(id);
            }
            if let Route::Settings(section) = route {
                value["state"] = json!({"tab":section});
            }
            value
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipping_pin_allowlist_redirects_and_session_ids_roundtrip() {
        let routes = decode(r#"[{"type":"sessions","id":"note"},{"type":"settings","state":{"tab":"audio"}},
            {"type":"contacts"},{"type":"shared_sessions","id":"share"},{"type":"shared_note_preview","id":"preview"},
            {"type":"empty"},{"type":"daily_summary","id":"daily"},{"type":"task","id":"task"}]"#).unwrap();
        assert_eq!(
            routes,
            vec![
                Route::Session(SessionId("note".into())),
                Route::settings("meetings"),
                Route::Contacts
            ]
        );
        assert_eq!(decode(&encode(&routes).unwrap()).unwrap(), routes);
        assert!(!encode(&routes).unwrap().contains("slot"));
    }

    #[test]
    fn malformed_pin_data_is_an_error_instead_of_an_empty_success() {
        assert!(decode("{broken").is_err());
        assert!(decode("{}").is_err());
        assert_eq!(
            decode(r#"[{"type":"ai"},{"type":"settings"}]"#).unwrap(),
            vec![Route::settings("transcription")]
        );
    }
}
