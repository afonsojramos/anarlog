use desktop_runtime::{Result, ServiceError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayAction {
    Open,
    StartMeeting,
    StopMeeting,
    Settings,
    Agenda,
    InstallUpdate,
    Hide,
    Quit,
}

#[derive(Clone, Debug)]
pub struct TrayState {
    pub recording: bool,
    pub update_ready: bool,
}

impl TrayState {
    pub fn permits(&self, action: TrayAction) -> bool {
        match action {
            TrayAction::StartMeeting => !self.recording,
            TrayAction::StopMeeting => self.recording,
            TrayAction::InstallUpdate => self.update_ready && !self.recording,
            _ => true,
        }
    }
}

pub trait TrayAdapter {
    fn update(&mut self, state: &TrayState) -> Result<()>;
}

pub struct UnavailableTray;

impl TrayAdapter for UnavailableTray {
    fn update(&mut self, _: &TrayState) -> Result<()> {
        Err(ServiceError::Unsupported(
            "Native tray host has not been attached to the GPUI event loop".into(),
        ))
    }
}
