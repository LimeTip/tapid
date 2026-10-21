use crate::{HttpResponse, MetadataError, RegistryClientError};

pub(crate) fn json_response(response: &HttpResponse) -> Result<&[u8], RegistryClientError> {
    if !(200..=203).contains(&response.status) {
        return Err(RegistryClientError::Metadata(MetadataError::HttpStatus(
            response.status,
        )));
    }
    let Some(content_type) = response.content_type.as_deref() else {
        return Err(RegistryClientError::Metadata(
            MetadataError::UnsupportedContentType("missing content type".into()),
        ));
    };
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    let normalized_media_type = media_type.to_ascii_lowercase();
    if normalized_media_type != "application/json" && !normalized_media_type.ends_with("+json") {
        return Err(RegistryClientError::Metadata(
            MetadataError::UnsupportedContentType(content_type.into()),
        ));
    }
    Ok(&response.body)
}

pub(crate) fn json_object(
    body: &[u8],
) -> Result<serde_json::Map<String, serde_json::Value>, MetadataError> {
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|error| MetadataError::InvalidJson(error.to_string()))?;
    match value {
        serde_json::Value::Object(object) => Ok(object),
        _ => Err(MetadataError::InvalidJson(
            "metadata must be an object".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_requires_success_status_and_json_content_type() {
        let status = HttpResponse {
            status: 503,
            content_type: Some("application/json".into()),
            body: b"{}".to_vec(),
        };
        assert!(matches!(
            json_response(&status),
            Err(RegistryClientError::Metadata(MetadataError::HttpStatus(
                503
            )))
        ));

        let missing_type = HttpResponse {
            status: 200,
            content_type: None,
            body: b"{}".to_vec(),
        };
        assert!(matches!(
            json_response(&missing_type),
            Err(RegistryClientError::Metadata(
                MetadataError::UnsupportedContentType(_)
            ))
        ));

        let wrong_type = HttpResponse {
            status: 200,
            content_type: Some("text/plain".into()),
            body: b"{}".to_vec(),
        };
        assert!(matches!(
            json_response(&wrong_type),
            Err(RegistryClientError::Metadata(
                MetadataError::UnsupportedContentType(_)
            ))
        ));
    }

    #[test]
    fn response_accepts_json_media_types_and_suffixes() {
        for content_type in [
            "application/json",
            "application/json; charset=utf-8",
            "application/vnd.example+json",
        ] {
            let response = HttpResponse {
                status: 200,
                content_type: Some(content_type.into()),
                body: b"{}".to_vec(),
            };
            assert_eq!(json_response(&response).unwrap(), b"{}");
        }
    }

    #[test]
    fn response_accepts_http_success_statuses_through_203() {
        for status in 200..=203 {
            let response = HttpResponse {
                status,
                content_type: Some("application/json".into()),
                body: b"{}".to_vec(),
            };
            assert_eq!(json_response(&response).unwrap(), b"{}");
        }
        let no_content = HttpResponse {
            status: 204,
            content_type: Some("application/json".into()),
            body: b"{}".to_vec(),
        };
        assert!(matches!(
            json_response(&no_content),
            Err(RegistryClientError::Metadata(MetadataError::HttpStatus(
                204
            )))
        ));
    }

    #[test]
    fn response_matches_json_media_types_case_insensitively() {
        for content_type in [
            "APPLICATION/JSON",
            "application/JSON; charset=utf-8",
            "application/vnd.example+JSON",
        ] {
            let response = HttpResponse {
                status: 200,
                content_type: Some(content_type.into()),
                body: b"{}".to_vec(),
            };
            assert_eq!(json_response(&response).unwrap(), b"{}");
        }
    }

    #[test]
    fn object_parser_rejects_malformed_json_and_non_object_roots() {
        assert!(matches!(
            json_object(b"{"),
            Err(MetadataError::InvalidJson(_))
        ));
        assert!(
            matches!(json_object(b"[]"), Err(MetadataError::InvalidJson(message)) if message == "metadata must be an object")
        );
        assert_eq!(
            json_object(b"{\"ok\":true}").unwrap().get("ok"),
            Some(&serde_json::Value::Bool(true))
        );
    }
}
