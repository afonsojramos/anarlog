use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "FileAddIcon.svg" => Some(include_bytes!("../../assets/FileAddIcon.svg")),
            "Search01Icon.svg" => Some(include_bytes!("../../assets/Search01Icon.svg")),
            "PencilEdit01Icon.svg" => Some(include_bytes!("../../assets/PencilEdit01Icon.svg")),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, _: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok([
            "FileAddIcon.svg",
            "Search01Icon.svg",
            "PencilEdit01Icon.svg",
        ]
        .into_iter()
        .map(Into::into)
        .collect())
    }
}
