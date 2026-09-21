use desktop_runtime::ServiceError;

#[derive(Clone, Debug)]
pub enum PlatformEvent {
    DeepLink(String),
    Shortcut(String),
    OpenMainWindow,
    Failed(ServiceError),
}

#[derive(Clone, Copy, Debug)]
pub enum WindowRole {
    Main,
    Settings,
    MeetingOverlay,
}
