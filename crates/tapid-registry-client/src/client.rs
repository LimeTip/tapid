use crate::{ClientError, FetchMode, RawRegistrySnapshot, RegistrySnapshot};
use tapid_core::RegistryOrigin;

pub trait RegistryTransport {
    type Error: std::error::Error + Send + Sync + 'static;
    fn fetch(&self, registry: &RegistryOrigin) -> Result<RawRegistrySnapshot, Self::Error>;
}

pub struct RegistryClient<T> {
    transport: T,
}
impl<T> RegistryClient<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}
impl<T: RegistryTransport> RegistryClient<T> {
    pub fn snapshot(
        &self,
        registry: &RegistryOrigin,
        mode: FetchMode,
    ) -> Result<RegistrySnapshot, ClientError<T::Error>> {
        if mode != FetchMode::Online {
            return Err(ClientError::NetworkDisabled(mode));
        }
        RegistrySnapshot::normalize(
            self.transport
                .fetch(registry)
                .map_err(ClientError::Transport)?,
        )
        .map_err(ClientError::Metadata)
    }
}
