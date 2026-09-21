use desktop_runtime::{Profile, Reply, RuntimeHandle};

pub fn start(profile: Profile) -> std::io::Result<(RuntimeHandle, Reply<()>)> {
    RuntimeHandle::start(profile)
}
