use crate::transport::request_url_is_safe;
use crate::{HttpResponse, HttpTransport, MetadataError, RegistryClientError, TransportError};
use url::Url;

pub(crate) fn download_artifact<T: HttpTransport>(
    transport: &T,
    artifact_url: &str,
) -> Result<HttpResponse, RegistryClientError> {
    let url = Url::parse(artifact_url).map_err(|_| {
        RegistryClientError::Metadata(MetadataError::InvalidArtifact(artifact_url.into()))
    })?;
    if !request_url_is_safe(artifact_url, &url) {
        return Err(RegistryClientError::Transport(
            TransportError::OriginNotAllowed(artifact_url.into()),
        ));
    }
    let response = transport
        .get(artifact_url)
        .map_err(RegistryClientError::Transport)?;
    if response.status != 200 {
        return Err(RegistryClientError::Metadata(MetadataError::HttpStatus(
            response.status,
        )));
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        response: HttpResponse,
        url: String,
    }
    impl HttpTransport for Fake {
        fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
            assert_eq!(url, self.url);
            Ok(self.response.clone())
        }
    }

    #[test]
    fn artifact_download_rejects_non_https_before_transport() {
        let transport = Fake {
            response: HttpResponse {
                status: 200,
                content_type: None,
                body: vec![],
            },
            url: "unused".into(),
        };
        assert!(matches!(
            download_artifact(&transport, "http://evil.example/a.tgz"),
            Err(RegistryClientError::Transport(
                TransportError::OriginNotAllowed(_)
            ))
        ));
    }

    struct UnexpectedTransport;

    impl HttpTransport for UnexpectedTransport {
        fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
            panic!("transport must not receive unsafe artifact URL: {url}");
        }
    }

    #[test]
    fn artifact_download_rejects_unsafe_urls_before_transport() {
        for url in [
            "https://user:secret@registry.npmjs.org/a.tgz",
            "https://@registry.npmjs.org/a.tgz",
            "https:\\@registry.npmjs.org/a.tgz",
            "https:\t//@registry.npmjs.org/a.tgz",
            "https:\n//@registry.npmjs.org/a.tgz",
            "https:\r//@registry.npmjs.org/a.tgz",
            "https:////@registry.npmjs.org/a.tgz",
            "https:/@registry.npmjs.org/a.tgz",
            "https://\u{200b}registry.npmjs.org/a.tgz",
            "https://registry.npmjs.org/\u{85}a.tgz",
            "https://registry.npmjs.org/\u{a0}a.tgz",
            " https://@registry.npmjs.org/a.tgz",
            "https://@registry.npmjs.org/a.tgz ",
            "https://registry.npmjs.org/a.tgz?token=value",
            "https://registry.npmjs.org/a.tgz#fragment",
        ] {
            assert!(
                matches!(
                    download_artifact(&UnexpectedTransport, url),
                    Err(RegistryClientError::Transport(
                        TransportError::OriginNotAllowed(_)
                    ))
                ),
                "{url}"
            );
        }
    }

    #[test]
    fn artifact_download_requires_status_200() {
        let url = "https://registry.npmjs.org/a.tgz";
        let transport = Fake {
            response: HttpResponse {
                status: 206,
                content_type: Some("application/json; charset=utf-8".into()),
                body: vec![],
            },
            url: url.into(),
        };
        assert!(matches!(
            download_artifact(&transport, url),
            Err(RegistryClientError::Metadata(MetadataError::HttpStatus(
                206
            )))
        ));
    }
}
