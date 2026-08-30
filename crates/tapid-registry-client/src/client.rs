use crate::{ClientError, FetchMode, MetadataError, RawRegistrySnapshot, RegistrySnapshot};
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
        let snapshot = RegistrySnapshot::normalize(
            self.transport
                .fetch(registry)
                .map_err(ClientError::Transport)?,
        )
        .map_err(ClientError::Metadata)?;
        if snapshot.registry() != registry {
            return Err(ClientError::Metadata(MetadataError::ConflictingField(
                "registry".into(),
            )));
        }
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MismatchedRegistryTransport;

    impl RegistryTransport for MismatchedRegistryTransport {
        type Error = std::convert::Infallible;

        fn fetch(&self, _registry: &RegistryOrigin) -> Result<RawRegistrySnapshot, Self::Error> {
            Ok(RawRegistrySnapshot {
                registry: "https://unexpected.example".into(),
                packages: vec![],
            })
        }
    }

    #[test]
    fn snapshot_rejects_registry_identity_mismatch() {
        let requested: RegistryOrigin = "https://registry.example".parse().unwrap();
        let error = RegistryClient::new(MismatchedRegistryTransport)
            .snapshot(&requested, FetchMode::Online)
            .unwrap_err();

        assert!(matches!(
            error,
            ClientError::Metadata(MetadataError::ConflictingField(field)) if field == "registry"
        ));
    }
}
