use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Collection {
    Versions,
    Tags,
}

pub(crate) struct PageRequest<'a> {
    pub(crate) size: usize,
    pub(crate) versions_after: Option<&'a str>,
    pub(crate) tags_after: Option<&'a str>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    order: u8,
    collection: Collection,
    next: usize,
    identity: [u8; 32],
    selector: [u8; 32],
    content: [u8; 32],
}

pub(crate) struct Pages {
    pub(crate) size: usize,
    pub(crate) versions_start: usize,
    pub(crate) tags_start: usize,
    identity: [u8; 32],
    selector: [u8; 32],
    content: [u8; 32],
}

impl Pages {
    pub(crate) fn new(
        inventory: &NpmInventory,
        selector: &str,
        request: PageRequest<'_>,
    ) -> Result<Self, OperationalError> {
        let identity = serde_json::to_vec(&(
            "npm",
            inventory.origin.to_string(),
            inventory.name.to_string(),
        ))
        .expect("identity tuple");
        let mut pages = Self {
            size: request.size,
            versions_start: 0,
            tags_start: 0,
            identity: Sha256::digest(identity).into(),
            selector: Sha256::digest(selector).into(),
            content: *inventory.metadata_digest(),
        };
        pages.versions_start = pages.start(
            request.versions_after,
            Collection::Versions,
            inventory.versions().len(),
        )?;
        pages.tags_start =
            pages.start(request.tags_after, Collection::Tags, inventory.tags().len())?;
        Ok(pages)
    }

    fn start(
        &self,
        token: Option<&str>,
        collection: Collection,
        total: usize,
    ) -> Result<usize, OperationalError> {
        let invalid = || {
            OperationalError::new(
                ErrorKind::RegistryContinuation,
                "invalid registry continuation",
            )
        };
        let Some(token) = token else {
            return Ok(0);
        };
        if token.len() > 2048 {
            return Err(invalid());
        }
        let bytes = URL_SAFE_NO_PAD.decode(token).map_err(|_| invalid())?;
        let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if cursor.version != 1
            || cursor.order != 1
            || cursor.collection != collection
            || cursor.identity != self.identity
            || cursor.selector != self.selector
        {
            return Err(invalid());
        }
        if cursor.content != self.content {
            return Err(OperationalError::new(
                ErrorKind::RegistryContinuationStale,
                "metadata changed since continuation was created",
            ));
        }
        if cursor.next == 0 || cursor.next >= total {
            return Err(invalid());
        }
        Ok(cursor.next)
    }

    pub(crate) fn next(
        &self,
        collection: Collection,
        returned: usize,
        total: usize,
    ) -> Option<String> {
        let start = match collection {
            Collection::Versions => self.versions_start,
            Collection::Tags => self.tags_start,
        };
        let next = start + returned;
        (next < total).then(|| {
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&Cursor {
                    version: 1,
                    order: 1,
                    collection,
                    next,
                    identity: self.identity,
                    selector: self.selector,
                    content: self.content,
                })
                .expect("fixed cursor"),
            )
        })
    }
}
